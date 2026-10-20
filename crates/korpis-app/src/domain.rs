// SPDX-License-Identifier: AGPL-3.0-or-later

//! Domains organizations hold (§3.2 of docs/07-networking.md). One is asked for under
//! `domain.write` on its organization and verified one of three ways: by DNS, the control plane
//! finding its token in a TXT record; by delegation, beneath a verified domain of an ancestor
//! organization, under `domain.write` on that ancestor too; or by attestation, under
//! `domain.attest` on the installation's root organization. A delegated or attested domain is
//! verified as it is made. Verification takes the domains lock and refuses a name overlapping
//! another organization's verified domain, save the domains it was delegated beneath.

mod verify;

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, DomainWrite as _, EffectWrite as _, RecipeSource, Store, StoreError, Unique,
};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::dns::domain::{Domain, DomainName, Verification, blocked_by, new_token};
use uuid::Uuid;

const OPERATION: &str = "domain.create";

/// How a new domain is to be verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requested {
    /// By a TXT record, later.
    DnsTxt,
    /// Beneath `parent`, a verified domain of an ancestor organization.
    Delegated {
        /// The domain it lies beneath.
        parent: Uuid,
    },
    /// By the caller's attestation, as an operator.
    Operator,
}

/// A domain to ask for.
#[derive(Debug, Clone)]
pub struct CreateDomain {
    /// The organization that will hold it.
    pub organization: Uuid,
    /// The name.
    pub name: DomainName,
    /// How it is verified.
    pub how: Requested,
}

/// The conflict of a change to domains that would leave `name`, which an endpoint routes,
/// without the organization routing it as its holder.
pub(crate) fn routed(name: &DomainName) -> AppError {
    AppError::conflict(
        "domain.routed",
        format!(
            "{} is routed by an endpoint, whose organization would no longer hold it; change the \
             endpoint first",
            name.as_str()
        ),
    )
}

/// The conflict of a name another organization holds.
pub(crate) fn held() -> AppError {
    AppError::conflict(
        "domain.held",
        "another organization holds a verified domain covering this name or beneath it",
    )
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The domains `parent` lies beneath, itself first, by delegation.
    pub(crate) async fn delegation_chain(
        &self,
        parent: Option<Uuid>,
    ) -> Result<Vec<Uuid>, AppError> {
        let mut chain = Vec::new();
        let mut next = parent;
        while let Some(id) = next.filter(|id| !chain.contains(id)) {
            chain.push(id);
            next = self.store.domain(id).await?.and_then(|d| d.parent);
        }
        Ok(chain)
    }

    /// The installation's root organization, above `organization`.
    async fn root_of(&self, organization: Uuid) -> Result<Uuid, AppError> {
        let chain = self.store.ancestry(ObjectRef::new(Kind::Organization, organization)).await?;
        chain.last().map(|n| n.object.id).ok_or(AppError::NotFound { kind: "organization" })
    }

    /// Checks a delegation of `cmd` beneath `parent`: verified, of a strict ancestor of the new
    /// holder, under `domain.write` there, and covering the name.
    async fn delegable(
        &self,
        caller: &Caller,
        cmd: &CreateDomain,
        parent: Uuid,
    ) -> Result<Uuid, AppError> {
        let p = self.store.domain(parent).await?.ok_or(AppError::NotFound { kind: "domain" })?;
        let permit = self
            .authorize(caller, "domain.write", ObjectRef::new(Kind::Organization, p.organization))
            .await?;
        let ancestry =
            self.store.ancestry(ObjectRef::new(Kind::Organization, cmd.organization)).await?;
        let above = ancestry.iter().skip(1).any(|n| n.object.id == p.organization);
        if !p.verified() || !above || !p.name.covers(&cmd.name) || p.name == cmd.name {
            return Err(AppError::invalid(
                "parent",
                "a verified domain of an ancestor organization, covering the name and not it",
            ));
        }
        Ok(permit.grant)
    }

    /// How `cmd` is to be verified, and the grant that allows it: `domain.write` on the holder,
    /// held beside `domain.write` on the parent's holder for a delegation; `domain.attest` on the
    /// root organization alone for an attestation, which is created for any organization in its
    /// subtree (§5 of docs/10-api.md).
    async fn verification_of(
        &self,
        caller: &Caller,
        cmd: &CreateDomain,
    ) -> Result<(Verification, Option<Uuid>, Uuid), AppError> {
        let org = ObjectRef::new(Kind::Organization, cmd.organization);
        Ok(match cmd.how {
            Requested::DnsTxt => {
                let permit = self.authorize(caller, "domain.write", org).await?;
                (Verification::DnsTxt, None, permit.grant)
            }
            Requested::Delegated { parent } => {
                let permit = self.authorize(caller, "domain.write", org).await?;
                self.delegable(caller, cmd, parent).await?;
                (Verification::Delegated, Some(parent), permit.grant)
            }
            Requested::Operator => {
                let root =
                    ObjectRef::new(Kind::Organization, self.root_of(cmd.organization).await?);
                let permit = self.authorize(caller, "domain.attest", root).await?;
                (Verification::Operator, None, permit.grant)
            }
        })
    }

    /// Asks for a domain, verified at once if delegated or attested.
    pub async fn create_domain(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Domain>,
        cmd: CreateDomain,
    ) -> Result<Reply<Domain>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let (verification, parent, grant) = self.verification_of(caller, &cmd).await?;
        let now = self.clock.now();
        let created = Domain {
            id: Uuid::now_v7(),
            organization: cmd.organization,
            name: cmd.name,
            verification,
            token: if verification == Verification::DnsTxt {
                new_token().map_err(|e| AppError::Internal(e.to_string()))?
            } else {
                String::new()
            },
            parent,
            verified_at: (verification != Verification::DnsTxt).then_some(now),
            created_at: now,
        };
        let chain = self.delegation_chain(parent).await?;
        let mut tx = self.store.begin().await?;
        if created.verified() {
            tx.lock_domains().await?;
            let others = tx.verified_overlapping(&created.name).await?;
            if blocked_by(&created, &others, &chain).is_some() {
                return Err(held());
            }
            // A delegation makes its holder the holder of every name beneath it, so it may not
            // take one that another organization's endpoint routes (§3.2 of
            // docs/07-networking.md).
            let beneath = tx.routes_beneath(&created.name).await?;
            if let Some(r) = beneath.iter().find(|r| r.organization != created.organization) {
                return Err(routed(&r.name));
            }
        }
        tx.insert_domain(&created).await.map_err(|e| match e {
            StoreError::Duplicate(Unique::DomainName) => {
                AppError::conflict("domain.taken", "the organization already asked for this name")
            }
            other => other.into(),
        })?;
        let effect =
            caller.effect("domain.create", Outcome::Succeeded).under(grant).after(details([
                ("domain", created.id.to_string().into()),
                ("organization", created.organization.to_string().into()),
                ("name", created.name.as_str().into()),
                ("verification", created.verification.as_str().into()),
                ("verified", created.verified().into()),
            ]));
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, OPERATION, created).await
    }
}
