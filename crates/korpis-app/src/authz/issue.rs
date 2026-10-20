// SPDX-License-Identifier: AGPL-3.0-or-later

//! Issuing a grant: attenuating one the caller holds (§3.2 of docs/08-identity.md), to a named
//! holder, to an API token created to hold it (§3.7), to an account created to hold it and invited
//! to register its first factor (§9.6), or to whoever holds the share link it is answered as
//! (§6.1).

use super::Snapshot;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    AccountWrite, Clock, CredentialWrite, EffectWrite, GrantWrite, RecipeSource, Store, StoreError,
    Unique,
};
use korpis_core::account::record::{Account, INVITATION_LIFETIME, Invitation, display_name};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::credential::{ApiToken, Secret, SecretKind};
use korpis_core::authz::evaluate::{IssueError, issue};
use korpis_core::authz::link;
use korpis_core::authz::{Action, Conditions, Delegation, Grant, GrantRecord, Resource, Scope};
use korpis_core::principal::Principal;
use std::collections::BTreeSet;
use std::time::SystemTime;
use uuid::Uuid;

const OPERATION: &str = "authz.issue_grant";

/// The longest label a grant may carry, in characters.
pub const MAX_LABEL: usize = 200;

/// Who will hold a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// A principal that exists, or is named by an identity provider.
    Principal(Principal),
    /// An API token created in the same transaction, which exists only to hold the grant.
    NewToken,
    /// An account created in the same transaction, holding the grant and no factor, with an
    /// invitation to register its first (§9.6 of docs/08-identity.md).
    NewUser {
        /// How the account is shown.
        display_name: String,
        /// When the invitation stops working, at most seven days on; seven days if not said.
        invitation_expires_at: Option<SystemTime>,
    },
    /// Whoever holds the share link the grant is answered as: a link-form grant, which must
    /// expire and limit its uses, on one object, and can never delegate (§6.1).
    Link,
}

/// A grant to issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueGrant {
    /// The grant to attenuate, which the caller must hold; with none named, the one grant the
    /// caller holds that the attenuation is valid from.
    pub parent: Option<Uuid>,
    /// Who will hold the new grant.
    pub holder: Holder,
    /// What it permits; at least one.
    pub actions: BTreeSet<Action>,
    /// Where.
    pub scope: Scope,
    /// Under which conditions.
    pub conditions: Conditions,
    /// What the holder may pass on.
    pub delegation: Delegation,
    /// What it is for, for people; required for a new token, which it names. From a template,
    /// the template's name when not given.
    pub label: String,
    /// The template it is issued from, which then says its actions, delegation, and scope from
    /// the object `scope` names (§4 of docs/08-identity.md).
    pub template: Option<super::FromTemplate>,
}

/// An issued grant, and the secret of the token created to hold it, if one was.
#[derive(Debug)]
pub struct Issued {
    /// The grant.
    pub record: GrantRecord,
    /// The new token's secret, answered once and stored only as its digest.
    pub secret: Option<Secret>,
    /// The share link, answered once and stored nowhere.
    pub link: Option<String>,
    /// The new account's invitation, answered once and stored only as its digest.
    pub invitation: Option<Secret>,
}

/// What an issue creates to hold the grant, beside it.
enum Created {
    /// Nothing: the holder exists, or is a link.
    Nothing,
    /// An API token, and its secret.
    Token(ApiToken, Secret),
    /// An account, its invitation, and the invitation's secret.
    User(Account, Invitation, Secret),
}

fn issue_error(e: &IssueError) -> AppError {
    match e {
        IssueError::NotHolder => AppError::invalid("parent", "the caller does not hold it"),
        other => AppError::invalid("grant", other.to_string()),
    }
}

/// Checks what the request asks for before anything is read.
fn validate(cmd: &IssueGrant) -> Result<(), AppError> {
    if cmd.actions.is_empty() {
        return Err(AppError::invalid("actions", "at least one action"));
    }
    if cmd.label.chars().count() > MAX_LABEL || cmd.label.chars().any(char::is_control) {
        return Err(AppError::invalid("label", "at most 200 characters, none of them control"));
    }
    match &cmd.holder {
        Holder::Principal(Principal::Link(_)) => {
            Err(AppError::invalid("subject", "a link's holder is made by issuing a link"))
        }
        Holder::Principal(Principal::Anonymous) => {
            Err(AppError::invalid("subject", "no grant is held by whoever failed to authenticate"))
        }
        Holder::Principal(p) => {
            p.validate().map_err(|e| AppError::invalid("subject", e.to_string()))
        }
        Holder::Link => link_conditions(cmd),
        Holder::NewToken if cmd.label.trim().is_empty() => {
            Err(AppError::invalid("label", "a new token is named by its label"))
        }
        Holder::NewToken => Ok(()),
        Holder::NewUser { display_name: name, .. } => display_name(name)
            .map(|_| ())
            .map_err(|e| AppError::invalid("display_name", e.to_string())),
    }
}

/// The account and invitation a grant to a new user creates, issued by `caller` at `now`.
fn new_user(
    caller: &Caller,
    name: &str,
    expires_at: Option<SystemTime>,
    now: SystemTime,
) -> Result<(Account, Invitation, Secret), AppError> {
    let latest = now + INVITATION_LIFETIME;
    let expires_at = expires_at.unwrap_or(latest);
    if expires_at <= now || expires_at > latest {
        return Err(AppError::invalid("invitation_expires_at", "within the next seven days"));
    }
    let account = Account {
        id: Uuid::now_v7(),
        display_name: display_name(name)
            .map_err(|e| AppError::invalid("display_name", e.to_string()))?,
        name: None,
        created_by: caller.principal.key(),
        created_at: now,
        disabled_at: None,
    };
    let invitation = Invitation {
        id: Uuid::now_v7(),
        account: account.id,
        issued_by: caller.principal.key(),
        expires_at,
        passkey_only: false,
        redeemed_at: None,
    };
    let secret = Secret::generate_kind(SecretKind::Invitation)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok((account, invitation, secret))
}

/// What a link must say of itself (§6.1 of docs/08-identity.md): short expiry and a use cap
/// are defaults, not options, so a link without either is refused rather than given one here,
/// where the issuer would not see it; and it is for one object, not a selector, because the
/// person it is sent to is shown exactly what it reaches.
fn link_conditions(cmd: &IssueGrant) -> Result<(), AppError> {
    if cmd.conditions.expires_at.is_none() {
        return Err(AppError::invalid("conditions.expires_at", "a link expires"));
    }
    if cmd.conditions.max_uses.is_none_or(|m| m == 0) {
        return Err(AppError::invalid("conditions.max_uses", "a link has a use cap"));
    }
    if !matches!(cmd.scope, Scope::Object(_)) {
        return Err(AppError::invalid("scope", "a link is for one object"));
    }
    Ok(())
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Issues a grant attenuated from one the caller holds.
    pub async fn issue_grant(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Issued>,
        mut cmd: IssueGrant,
    ) -> Result<Reply<Issued>, AppError> {
        caller.direct()?;
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let from_template = self.apply_template(caller, &mut cmd).await?;
        validate(&cmd)?;
        let now = self.clock.now();
        let id = Uuid::now_v7();
        let (holder, created) = match cmd.holder {
            Holder::Principal(p) => (p, Created::Nothing),
            Holder::Link => (Principal::Link(id), Created::Nothing),
            Holder::NewToken => {
                let token = ApiToken {
                    id: Uuid::now_v7(),
                    name: cmd.label.clone(),
                    expires_at: cmd.conditions.expires_at,
                };
                let secret = Secret::generate().map_err(|e| AppError::Internal(e.to_string()))?;
                (Principal::Token(token.id), Created::Token(token, secret))
            }
            Holder::NewUser { display_name, invitation_expires_at } => {
                let (account, invitation, secret) =
                    new_user(caller, &display_name, invitation_expires_at, now)?;
                (Principal::User(account.id), Created::User(account, invitation, secret))
            }
        };
        let mut record = GrantRecord {
            grant: Grant {
                id,
                subject: holder.subject(),
                actions: cmd.actions,
                scope: cmd.scope,
                conditions: cmd.conditions,
                parent: cmd.parent,
                delegation: cmd.delegation,
                link_form: matches!(holder, Principal::Link(_)),
                uses: 0,
                revoked: false,
                suspended: false,
                // Set by the binding of an external subject, never by the request.
                irreversible_override: false,
                root: false,
            },
            holder,
            issued_by: caller.principal.clone(),
            issued_at: now,
            revoked_at: None,
            suspended_at: None,
            label: cmd.label,
            from_template,
        };
        self.vouched(&mut record).await?;
        let parent = self.parent_for(caller, &mut record.grant).await?;
        self.record_issue(caller, idem, record, &parent, created).await
    }

    /// Writes an issued grant, the token or account created to hold it, and its effect.
    async fn record_issue(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Issued>,
        record: GrantRecord,
        parent: &Grant,
        created: Created,
    ) -> Result<Reply<Issued>, AppError> {
        let mut tx = self.store.begin().await?;
        // A child takes its uses from a use-limited parent under compare-and-swap (§6.2), so two
        // concurrent attenuations cannot both spend the same uses.
        if let (Some(max), Some(taken)) =
            (parent.conditions.max_uses, record.grant.conditions.max_uses)
            && !tx.take_uses(parent.id, taken, max).await?
        {
            return Err(AppError::conflict("grant.uses_taken", "the parent's uses changed"));
        }
        match &created {
            Created::Nothing => {}
            Created::Token(t, secret) => {
                tx.insert_token(t, &secret.digest(), &caller.principal).await?;
            }
            Created::User(account, invitation, secret) => {
                tx.insert_account(account).await?;
                tx.insert_invitation(invitation, &secret.digest()).await?;
                let effect = caller
                    .effect("account.invited", Outcome::Succeeded)
                    .under(parent.id)
                    .after(details([
                        ("account", account.id.to_string().into()),
                        ("display_name", account.display_name.clone().into()),
                        ("invitation", invitation.id.to_string().into()),
                    ]));
                tx.append_effect(&effect).await?;
            }
        }
        tx.insert_grant(&record).await.map_err(|e| match e {
            StoreError::Duplicate(Unique::Other) => {
                AppError::conflict("grant.exists", "the grant exists")
            }
            other => other.into(),
        })?;
        let effect =
            caller.effect("grant.issued", Outcome::Succeeded).under(parent.id).after(details([
                ("grant", record.grant.id.to_string().into()),
                ("parent", parent.id.to_string().into()),
                ("holder", record.holder.key().into()),
                ("label", record.label.clone().into()),
                ("template", record.from_template.map(|t| t.template.to_string()).into()),
                ("template_version", record.from_template.map(|t| t.version).into()),
            ]));
        tx.append_effect(&effect).await?;
        let (secret, invitation) = match created {
            Created::Nothing => (None, None),
            Created::Token(_, secret) => (Some(secret), None),
            Created::User(.., secret) => (None, Some(secret)),
        };
        let link = match (&record.holder, record.grant.conditions.expires_at) {
            (Principal::Link(grant), Some(expires_at)) => Some(
                link::mint(&self.token_key, *grant, expires_at)
                    .map_err(|e| AppError::Internal(e.to_string()))?,
            ),
            _ => None,
        };
        finish(tx, caller, idem, OPERATION, Issued { record, secret, link, invitation }).await
    }

    /// The parent `child` is attenuated from, checked: the one it names, or else the one grant
    /// the caller holds that it is valid from, which is then named on it.
    async fn parent_for(&self, caller: &Caller, child: &mut Grant) -> Result<Grant, AppError> {
        if child.parent.is_some() {
            return self.check_issue(caller, child).await;
        }
        let held = self.store.chains_held_by(&caller.subject.key).await?;
        let leaves: Vec<Uuid> = held.iter().filter_map(|c| c.first().map(|r| r.grant.id)).collect();
        let (mut valid, mut refused) = (Vec::new(), None);
        for leaf in &leaves {
            child.parent = Some(*leaf);
            match self.check_issue(caller, child).await {
                Ok(p) => valid.push(p),
                Err(e @ (AppError::Invalid { .. } | AppError::NotFound { .. })) => {
                    refused = Some(e);
                }
                Err(other) => return Err(other),
            }
        }
        match (valid.pop(), valid.len(), refused) {
            (Some(p), 0, _) => {
                child.parent = Some(p.id);
                Ok(p)
            }
            (Some(_), n, _) => Err(AppError::invalid(
                "parent",
                format!("{} grants you hold can be attenuated into it; name one", n + 1),
            )),
            (None, _, Some(e)) if leaves.len() == 1 => Err(e),
            (None, ..) => Err(AppError::invalid(
                "parent",
                format!("none of the {} grants you hold can be attenuated into it", leaves.len()),
            )),
        }
    }

    /// Checks the attenuation against the parent's chain as it stands; returns the parent.
    pub(crate) async fn check_issue(
        &self,
        caller: &Caller,
        child: &Grant,
    ) -> Result<Grant, AppError> {
        self.check_issue_assuming(caller, child, None).await
    }

    /// [`Self::check_issue`], with `assumed` standing for an object the caller's transaction
    /// is about to create, such as the workload an installation runs as.
    pub(crate) async fn check_issue_assuming(
        &self,
        caller: &Caller,
        child: &Grant,
        assumed: Option<&Resource>,
    ) -> Result<Grant, AppError> {
        let parent_id = child.parent.ok_or(AppError::invalid("parent", "required"))?;
        let chain: Vec<Grant> =
            self.store.grant_chain(parent_id).await?.into_iter().map(|r| r.grant).collect();
        let Some(parent) = chain.first().cloned() else {
            return Err(AppError::NotFound { kind: "grant" });
        };
        let mut snapshot = Snapshot::default();
        if let Some(r) = assumed {
            snapshot.assume(r.clone());
        }
        for g in chain.iter().chain(std::iter::once(child)) {
            snapshot.load_grant(&self.store, g).await?;
        }
        let allows = self.binding_allows_irreversible(child).await?;
        issue(&caller.subject, &chain, child, allows, &snapshot, self.clock.now())
            .map_err(|e| issue_error(&e))?;
        Ok(parent)
    }
}
