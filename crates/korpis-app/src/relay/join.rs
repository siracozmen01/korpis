// SPDX-License-Identifier: AGPL-3.0-or-later

//! A relay joining (§3.1 of docs/02-architecture.md): it presents its enrollment token once with
//! the public half of its identity key, which it signs its requests to the control plane with,
//! and the public half of its TLS key, for which the installation's authority issues it a
//! certificate naming its URL's host. Agents and the CLI trust that authority already, so a
//! relay is trusted exactly as the control plane is. Asking again with the same token and
//! identity key returns the same relay with a fresh certificate.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{
    Clock, EffectWrite as _, IssuedCertificate, RecipeSource, RelayEnrollment, RelayWrite as _,
    StoreError, Unique, VerifyKey,
};
use crate::ports::{Store, Tx as _};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::node::identity::valid_public_key;
use korpis_core::principal::Principal;
use korpis_core::relay::Relay;
use std::net::IpAddr;
use std::time::Duration;
use uuid::Uuid;

/// How long a relay's certificate is valid at most. Each relay's is shortened by up to nine
/// days, fixed by its identifier, so relays that joined together do not expire together (§7 of
/// docs/18-operations.md).
pub const RELAY_CERTIFICATE_LIFETIME: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// A relay's join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinRelay {
    /// The enrollment token's secret.
    pub token: String,
    /// The relay identity key's public half: Ed25519, raw.
    pub identity_key: Vec<u8>,
    /// The relay TLS key's public half: an uncompressed P-256 point.
    pub tls_key: Vec<u8>,
    /// Where the relay connected from.
    pub source_ip: Option<IpAddr>,
}

/// What a joined relay is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinedRelay {
    /// The relay.
    pub relay: Relay,
    /// Its certificate.
    pub certificate: IssuedCertificate,
    /// The authority behind it, which agents and clients trust.
    pub authority_pem: String,
    /// Every key that ever signed a capability token.
    pub token_keys: Vec<VerifyKey>,
}

/// The lifetime of `relay`'s certificates.
pub(super) fn lifetime(relay: Uuid) -> Duration {
    let stagger = relay.as_u128() % (9 * 24 * 60 * 60);
    RELAY_CERTIFICATE_LIFETIME
        .saturating_sub(Duration::from_secs(u64::try_from(stagger).unwrap_or(0)))
}

fn join_conflict(e: StoreError) -> AppError {
    match e {
        StoreError::Duplicate(Unique::Relay) => AppError::conflict(
            "relay.taken",
            "a relay with this name, URL, or identity key has joined",
        ),
        other => other.into(),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Joins the relay a token describes.
    pub async fn join_relay(&self, cmd: JoinRelay) -> Result<JoinedRelay, AppError> {
        if !valid_public_key(&cmd.identity_key) {
            return Err(AppError::invalid("identity_key", "an Ed25519 public key, 32 bytes"));
        }
        super::renew::valid_tls_key(&cmd.tls_key)?;
        let issuer = self
            .certificate_issuer
            .clone()
            .ok_or_else(|| AppError::Internal("no certificate authority is configured".into()))?;
        let caller = self.authenticate(&cmd.token, cmd.source_ip).await?;
        let Principal::Token(token) = caller.principal else {
            return Err(AppError::Unauthenticated);
        };
        let enrollment =
            self.store.relay_enrollment(token).await?.ok_or_else(|| {
                AppError::invalid("enrollment_token", "not a relay enrollment token")
            })?;
        let relay = match enrollment.relay {
            Some(relay) => self.joined_again(relay, &cmd.identity_key).await?,
            None => self.join_new(&caller, &enrollment, &cmd.identity_key).await?,
        };
        let certificate = issuer
            .issue(relay.url.host(), &cmd.tls_key, lifetime(relay.id))
            .map_err(AppError::Internal)?;
        let mut tx = self.store.begin().await?;
        tx.record_relay_certificate(relay.id, certificate.expires_at).await?;
        tx.commit().await?;
        Ok(JoinedRelay {
            relay,
            certificate,
            authority_pem: issuer.authority_pem(),
            token_keys: self.store.verify_keys("token").await?,
        })
    }

    async fn joined_again(&self, relay: Uuid, identity_key: &[u8]) -> Result<Relay, AppError> {
        let key = self.store.relay_key(relay).await?;
        if key.as_deref() != Some(identity_key) {
            return Err(AppError::conflict(
                "enrollment.used",
                "this token joined a relay with another key",
            ));
        }
        self.store.relay(relay).await?.ok_or(AppError::NotFound { kind: "relay" })
    }

    async fn join_new(
        &self,
        caller: &Caller,
        enrollment: &RelayEnrollment,
        identity_key: &[u8],
    ) -> Result<Relay, AppError> {
        let root = ObjectRef::new(Kind::Organization, enrollment.organization);
        let permit = self.authorize(caller, "node.enroll", root).await?;
        let now = self.clock.now();
        let relay = Relay {
            id: Uuid::now_v7(),
            name: enrollment.relay_name.clone(),
            url: enrollment.relay_url.clone(),
            joined_at: now,
            certificate_expires_at: None,
            removed_at: None,
            held_at: None,
            last_seen_at: None,
        };
        let mut tx = self.store.begin().await?;
        if !tx.join_relay(&relay, identity_key, enrollment.token).await.map_err(join_conflict)? {
            return Err(AppError::conflict("enrollment.used", "the token was redeemed meanwhile"));
        }
        let effect = Effect::new(caller.principal.clone(), "relay.join", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("relay", relay.id.to_string().into()),
                ("name", relay.name.as_str().into()),
                ("url", relay.url.as_str().into()),
            ]));
        tx.append_effect(&effect).await?;
        tx.commit().await?;
        Ok(relay)
    }
}
