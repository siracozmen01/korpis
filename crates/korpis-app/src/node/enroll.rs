// SPDX-License-Identifier: AGPL-3.0-or-later

//! Enrolling a node (§3 of docs/18-operations.md, §7 of docs/02-architecture.md). An operator
//! creates a one-time token bound to an organization, a name, and labels; the agent presents it
//! once with the public half of the key it generated, and the node exists from then on.
//!
//! The token is not a special credential. It is a Token subject holding a grant of
//! `node.enroll` attenuated from the creator's, with `max_uses` 1, so it is redeemed online
//! like every use-limited grant and audited like every other one.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, CredentialWrite, EffectWrite, Enrollment, EnrollmentWrite, GrantWrite, NodeWrite,
    RecipeSource, Store, StoreError, Tx, Unique, VerifyKey,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::Scope;
use korpis_core::authz::credential::{ApiToken, Secret};
use korpis_core::authz::{Action, Conditions, Delegation, Grant, GrantRecord, Kind, ObjectRef};
use korpis_core::labels::Labels;
use korpis_core::node::identity::valid_public_key;
use korpis_core::node::{Connectivity, Node, NodeAdmin, Reachability};
use korpis_core::principal::Principal;
use korpis_core::tenancy::Name;
use korpis_core::tenancy::quota::Bounds;
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::Duration;
use uuid::Uuid;

const OPERATION: &str = "node.create_enrollment";

/// How long a token may be redeemed when the request does not say.
pub const DEFAULT_ENROLLMENT_TTL: Duration = Duration::from_secs(60 * 60);
/// The longest a token may be redeemed: it is a bearer credential for a node.
pub const MAX_ENROLLMENT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// An enrollment token to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateEnrollment {
    /// The organization the node will belong to.
    pub organization: Uuid,
    /// The node's name, unique in the installation.
    pub node_name: Name,
    /// The node's labels.
    pub labels: Labels,
    /// How long it may be redeemed; [`DEFAULT_ENROLLMENT_TTL`] when `None`.
    pub expires_in: Option<Duration>,
}

/// A created enrollment token.
#[derive(Debug, Clone)]
pub struct EnrollmentToken {
    /// What it binds.
    pub enrollment: Enrollment,
    /// The secret, shown once.
    pub secret: Secret,
}

/// An agent's enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enroll {
    /// The token's secret.
    pub token: String,
    /// The node identity key's public half.
    pub public_key: Vec<u8>,
    /// What the machine calls itself; informational.
    pub hostname: String,
    /// Where the agent connected from.
    pub source_ip: Option<IpAddr>,
}

/// What an enrolled agent is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrolled {
    /// The node.
    pub node: Uuid,
    /// Every key that ever signed an assignment.
    pub assignment_keys: Vec<VerifyKey>,
    /// Every key that ever signed a capability token.
    pub token_keys: Vec<VerifyKey>,
}

/// A one-use enrollment token's grant: `node.enroll` over one organization, attenuated from
/// the creator's, redeemable once before it expires. Nodes and relays are enrolled with it.
pub(crate) struct OneUse {
    /// The Token subject that holds it.
    pub(crate) token: Uuid,
    /// The grant.
    pub(crate) grant: Uuid,
    /// Where it enrolls.
    pub(crate) organization: Uuid,
    /// Who created it, and when.
    pub(crate) created: (Principal, std::time::SystemTime),
    /// When it may no longer be redeemed.
    pub(crate) expires_at: std::time::SystemTime,
    /// What it is for, for humans.
    pub(crate) label: String,
}

pub(crate) fn enrollment_grant(parent: Uuid, one: OneUse) -> Result<GrantRecord, AppError> {
    let enroll = Action::parse("node.enroll").map_err(|e| AppError::Internal(e.to_string()))?;
    let holder = Principal::Token(one.token);
    Ok(GrantRecord {
        grant: Grant {
            id: one.grant,
            issued_at: one.created.1,
            via: None,
            subject: holder.subject(),
            actions: BTreeSet::from([enroll]),
            scope: Scope::Object(ObjectRef::new(Kind::Organization, one.organization)),
            conditions: Conditions {
                expires_at: Some(one.expires_at),
                max_uses: Some(1),
                ..Conditions::default()
            },
            parent: Some(parent),
            delegation: Delegation::default(),
            link_form: false,
            uses: 0,
            revoked: false,
            suspended: false,
            irreversible_override: false,
            root: false,
        },
        holder,
        issued_by: one.created.0,
        issued_at: one.created.1,
        revoked_at: None,
        suspended_at: None,
        label: one.label,
        from_template: None,
    })
}

fn enrolled_node(e: &Enrollment, id: Uuid, now: std::time::SystemTime) -> Node {
    Node {
        id,
        name: e.node_name.clone(),
        organization: e.organization,
        labels: e.labels.clone(),
        taints: Vec::new(),
        capacity: Bounds::default(),
        allocatable: Bounds::default(),
        drivers: Vec::new(),
        devices: Vec::new(),
        unoffered_devices: Vec::new(),
        storage_classes: Vec::new(),
        reachability: Reachability::EdgeOnly,
        advertised: None,
        probed_at: None,
        admin: NodeAdmin::Active,
        connectivity: Connectivity::Registering,
        agent_version: None,
        last_seen: None,
        enrolled_at: now,
        version: 1,
    }
}

fn node_conflict(e: StoreError) -> AppError {
    match e {
        StoreError::Duplicate(Unique::NodeName) => {
            AppError::conflict("node.name_taken", "a node with this name is enrolled")
        }
        StoreError::Duplicate(Unique::NodeKey) => {
            AppError::conflict("node.key_taken", "this key already identifies a node")
        }
        other => other.into(),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Creates a one-time token that enrolls one node into an organization the caller may
    /// enroll nodes into.
    pub async fn create_enrollment(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, EnrollmentToken>,
        cmd: CreateEnrollment,
    ) -> Result<Reply<EnrollmentToken>, AppError> {
        caller.direct()?;
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let ttl = cmd.expires_in.unwrap_or(DEFAULT_ENROLLMENT_TTL);
        if ttl.is_zero() || ttl > MAX_ENROLLMENT_TTL {
            return Err(AppError::invalid("expires_in", "more than zero and at most a day"));
        }
        let org = ObjectRef::new(Kind::Organization, cmd.organization);
        let permit = self.authorize(caller, "node.enroll", org).await?;
        let now = self.clock.now();
        let enrollment = Enrollment {
            token: Uuid::now_v7(),
            grant: Uuid::now_v7(),
            organization: cmd.organization,
            node_name: cmd.node_name,
            labels: cmd.labels,
            created_by: caller.principal.clone(),
            created_at: now,
            expires_at: now + ttl,
            node: None,
        };
        let one = OneUse {
            token: enrollment.token,
            grant: enrollment.grant,
            organization: enrollment.organization,
            created: (enrollment.created_by.clone(), enrollment.created_at),
            expires_at: enrollment.expires_at,
            label: format!("enrollment of node {}", enrollment.node_name.as_str()),
        };
        let mut record = enrollment_grant(permit.grant, one)?;
        let parent = self.check_issue(caller, &mut record.grant).await?;
        let secret = Secret::generate().map_err(|e| AppError::Internal(e.to_string()))?;
        let token = ApiToken {
            id: enrollment.token,
            name: format!("enrollment of node {}", enrollment.node_name.as_str()),
            expires_at: Some(enrollment.expires_at),
        };
        let mut tx = self.store.begin().await?;
        if let Some(max) = parent.conditions.max_uses
            && !tx.take_uses(parent.id, 1, max).await?
        {
            return Err(AppError::conflict("grant.uses_taken", "the parent's uses changed"));
        }
        tx.insert_token(&token, &secret.digest(), &caller.principal).await?;
        tx.insert_grant(&record).await?;
        tx.insert_enrollment(&enrollment).await?;
        for effect in creation_effects(caller, permit.grant, &enrollment) {
            tx.append_effect(&effect).await?;
        }
        finish(tx, caller, idem, OPERATION, EnrollmentToken { enrollment, secret }).await
    }

    /// Enrolls the node a token describes, with the agent's public key. Asking again with the
    /// same token and key returns the same node.
    pub async fn enroll(&self, cmd: Enroll) -> Result<Enrolled, AppError> {
        if !valid_public_key(&cmd.public_key) {
            return Err(AppError::invalid("node_public_key", "an Ed25519 public key, 32 bytes"));
        }
        let caller = self.authenticate(&cmd.token, cmd.source_ip).await?;
        let Principal::Token(token) = caller.principal else {
            return Err(AppError::Unauthenticated);
        };
        let enrollment = self
            .store
            .enrollment(token)
            .await?
            .ok_or_else(|| AppError::invalid("enrollment_token", "not an enrollment token"))?;
        let node = match enrollment.node {
            Some(node) => self.enrolled_again(node, &cmd.public_key).await?,
            None => self.enroll_new(&caller, &enrollment, &cmd).await?,
        };
        Ok(Enrolled {
            node,
            assignment_keys: self.store.verify_keys("assignment").await?,
            token_keys: self.store.verify_keys("token").await?,
        })
    }

    async fn enrolled_again(&self, node: Uuid, public_key: &[u8]) -> Result<Uuid, AppError> {
        let key = self.store.node_key(node).await?;
        if key.as_deref() == Some(public_key) {
            return Ok(node);
        }
        Err(AppError::conflict("enrollment.used", "this token enrolled a node with another key"))
    }

    async fn enroll_new(
        &self,
        caller: &Caller,
        enrollment: &Enrollment,
        cmd: &Enroll,
    ) -> Result<Uuid, AppError> {
        let org = ObjectRef::new(Kind::Organization, enrollment.organization);
        let permit = self.authorize(caller, "node.enroll", org).await?;
        let now = self.clock.now();
        let node = enrolled_node(enrollment, Uuid::now_v7(), now);
        let mut tx = self.store.begin().await?;
        tx.enroll_node(&node, &cmd.public_key).await.map_err(node_conflict)?;
        if !tx.complete_enrollment(enrollment.token, node.id, now).await? {
            return Err(AppError::conflict("enrollment.used", "the token was redeemed meanwhile"));
        }
        let mut effect =
            caller.effect("node.enroll", Outcome::Succeeded).under(permit.grant).after(details([
                ("node", node.id.to_string().into()),
                ("name", node.name.as_str().into()),
                ("organization", node.organization.to_string().into()),
                ("hostname", cmd.hostname.clone().into()),
            ]));
        effect.node = Some(node.id);
        tx.append_effect(&effect).await?;
        tx.commit().await?;
        Ok(node.id)
    }
}

fn creation_effects(caller: &Caller, parent: Uuid, e: &Enrollment) -> [Effect; 2] {
    let home = ObjectRef::new(Kind::Organization, e.organization);
    [
        caller.effect("grant.issued", Outcome::Succeeded).under(parent).about(home).after(details(
            [
                ("grant", e.grant.to_string().into()),
                ("parent", parent.to_string().into()),
                ("holder", Principal::Token(e.token).key().into()),
            ],
        )),
        caller
            .effect("node.enrollment.create", Outcome::Succeeded)
            .under(parent)
            .about(home)
            .after(details([
                ("organization", e.organization.to_string().into()),
                ("node_name", e.node_name.as_str().into()),
                ("token", e.token.to_string().into()),
                ("grant", e.grant.to_string().into()),
            ])),
    ]
}

/// The first node's enrollment, made by installation in its own transaction (§2 of
/// docs/18-operations.md): attenuated from the root grant `parent`, which no one has exercised
/// yet, and recorded as the bootstrap actor, as the root itself is. It lasts the longest an
/// enrollment may, a day, since whoever installs joins the first node next.
pub(crate) async fn first_enrollment<T: Tx>(
    tx: &mut T,
    (organization, parent): (Uuid, Uuid),
    node_name: Name,
    now: std::time::SystemTime,
) -> Result<EnrollmentToken, AppError> {
    let bootstrap = Principal::System(korpis_core::principal::SystemActor::Bootstrap);
    let enrollment = Enrollment {
        token: Uuid::now_v7(),
        grant: Uuid::now_v7(),
        organization,
        node_name,
        labels: Labels::default(),
        created_by: bootstrap.clone(),
        created_at: now,
        expires_at: now + MAX_ENROLLMENT_TTL,
        node: None,
    };
    let label = format!("enrollment of node {}", enrollment.node_name.as_str());
    let one = OneUse {
        token: enrollment.token,
        grant: enrollment.grant,
        organization,
        created: (bootstrap.clone(), now),
        expires_at: enrollment.expires_at,
        label: label.clone(),
    };
    let record = enrollment_grant(parent, one)?;
    let secret = Secret::generate().map_err(|e| AppError::Internal(e.to_string()))?;
    let token =
        ApiToken { id: enrollment.token, name: label, expires_at: Some(enrollment.expires_at) };
    tx.insert_token(&token, &secret.digest(), &bootstrap).await?;
    tx.insert_grant(&record).await?;
    tx.insert_enrollment(&enrollment).await?;
    let by = Caller::new(bootstrap, None, false);
    for effect in creation_effects(&by, parent, &enrollment) {
        tx.append_effect(&effect).await?;
    }
    Ok(EnrollmentToken { enrollment, secret })
}
