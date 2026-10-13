// SPDX-License-Identifier: AGPL-3.0-or-later

//! Creating a relay's enrollment token: one-use, bound to the relay's name and URL, holding a
//! grant of `node.enroll` over the root organization attenuated from the creator's, exactly as a
//! node's is (§3 of docs/18-operations.md).

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::node::{DEFAULT_ENROLLMENT_TTL, MAX_ENROLLMENT_TTL, OneUse, enrollment_grant};
use crate::ports::{
    Clock, CredentialWrite as _, EffectWrite as _, GrantWrite as _, RecipeSource, RelayEnrollment,
    RelayWrite as _, Store,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::credential::{ApiToken, Secret};
use korpis_core::principal::Principal;
use korpis_core::relay::RelayUrl;
use korpis_core::tenancy::Name;
use std::time::Duration;
use uuid::Uuid;

const OPERATION: &str = "relay.create_enrollment";

/// A relay enrollment token to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRelayEnrollment {
    /// The root organization.
    pub organization: Uuid,
    /// The relay's name, unique in the installation.
    pub name: Name,
    /// Where clients and nodes will reach it; its certificate is issued for this host.
    pub url: RelayUrl,
    /// How long it may be redeemed; the node default when `None`.
    pub expires_in: Option<Duration>,
}

/// A created relay enrollment token.
#[derive(Debug, Clone)]
pub struct RelayEnrollmentToken {
    /// What it binds.
    pub enrollment: RelayEnrollment,
    /// The secret, shown once.
    pub secret: Secret,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Creates a one-time token that enrolls one relay, for a caller who may enroll nodes into
    /// the root organization.
    pub async fn create_relay_enrollment(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, RelayEnrollmentToken>,
        cmd: CreateRelayEnrollment,
    ) -> Result<Reply<RelayEnrollmentToken>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let ttl = cmd.expires_in.unwrap_or(DEFAULT_ENROLLMENT_TTL);
        if ttl.is_zero() || ttl > MAX_ENROLLMENT_TTL {
            return Err(AppError::invalid("expires_in", "more than zero and at most a day"));
        }
        let root = self.root(cmd.organization).await?;
        let permit = self.authorize(caller, "node.enroll", root).await?;
        let now = self.clock.now();
        let enrollment = RelayEnrollment {
            token: Uuid::now_v7(),
            organization: cmd.organization,
            grant: Uuid::now_v7(),
            relay_name: cmd.name,
            relay_url: cmd.url,
            created_by: caller.principal.clone(),
            created_at: now,
            expires_at: now + ttl,
            relay: None,
        };
        let label = format!("enrollment of relay {}", enrollment.relay_name.as_str());
        let one = OneUse {
            token: enrollment.token,
            grant: enrollment.grant,
            organization: cmd.organization,
            created: (enrollment.created_by.clone(), now),
            expires_at: enrollment.expires_at,
            label: label.clone(),
        };
        let record = enrollment_grant(&permit, one)?;
        let parent = self.check_issue(caller, &record.grant).await?;
        let secret = Secret::generate().map_err(|e| AppError::Internal(e.to_string()))?;
        let token =
            ApiToken { id: enrollment.token, name: label, expires_at: Some(enrollment.expires_at) };
        let mut tx = self.store.begin().await?;
        if let Some(max) = parent.conditions.max_uses
            && !tx.take_uses(parent.id, 1, max).await?
        {
            return Err(AppError::conflict("grant.uses_taken", "the parent's uses changed"));
        }
        tx.insert_token(&token, &secret.digest(), &caller.principal).await?;
        tx.insert_grant(&record).await?;
        tx.insert_relay_enrollment(&enrollment).await?;
        for effect in effects(caller, permit.grant, &enrollment) {
            tx.append_effect(&effect).await?;
        }
        finish(tx, caller, idem, OPERATION, RelayEnrollmentToken { enrollment, secret }).await
    }
}

/// What creating `e` records: the token's grant issued, and the enrollment created.
fn effects(caller: &Caller, under: Uuid, e: &RelayEnrollment) -> [Effect; 2] {
    let actor = caller.principal.clone();
    let issued = Effect::new(actor.clone(), "grant.issued", Outcome::Succeeded).under(under).after(
        details([
            ("grant", e.grant.to_string().into()),
            ("parent", under.to_string().into()),
            ("holder", Principal::Token(e.token).key().into()),
        ]),
    );
    let created = Effect::new(actor, OPERATION, Outcome::Succeeded).under(under).after(details([
        ("name", e.relay_name.as_str().into()),
        ("url", e.relay_url.as_str().into()),
        ("token", e.token.to_string().into()),
        ("grant", e.grant.to_string().into()),
    ]));
    [issued, created]
}
