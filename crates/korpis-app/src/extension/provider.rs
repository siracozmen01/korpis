// SPDX-License-Identifier: AGPL-3.0-or-later

//! Provider registrations (§3.3 of docs/16-extensions.md). An installed extension opens a
//! stream, authenticated as itself, and registers one of the `provides` entries its installation
//! recorded; the replica holding the stream keeps the registration seen while it is open, and a
//! registration nobody has seen for a while belongs to a replica that is gone and is withdrawn.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{
    Clock, EffectWrite, ExtensionWrite as _, ProviderWrite, RecipeSource, Store, Tx,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::extension::{
    CallFailure, CallOutcome, MAX_CONCURRENCY, MAX_VERSION, ProvidedInterface, ProviderType,
    Registration,
};
use korpis_core::principal::{Principal, SystemActor};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// What a stream asks to serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Register {
    /// What kind of provider.
    pub kind: ProviderType,
    /// Which of its installation's `provides` entries.
    pub name: String,
    /// The provider's own version.
    pub version: String,
    /// How many calls it accepts at once.
    pub concurrency: u32,
    /// An identity provider's: it signs people in (§9.7 of docs/08-identity.md).
    pub sign_in: bool,
}

fn checked(r: &Register) -> Result<(), AppError> {
    let plain =
        |s: &str| !s.is_empty() && s.len() <= MAX_VERSION && !s.chars().any(char::is_control);
    if !plain(&r.version) {
        return Err(AppError::invalid("register.version", "a version, at most 64 bytes"));
    }
    if r.concurrency == 0 || r.concurrency > MAX_CONCURRENCY {
        return Err(AppError::invalid("register.concurrency", "1 to 1024 calls at once"));
    }
    if r.sign_in && r.kind != ProviderType::Identity {
        return Err(AppError::invalid("register.sign_in", "an identity provider's alone"));
    }
    Ok(())
}

fn effect(actor: Principal, action: &str, r: &Registration, workload: Option<Uuid>) -> Effect {
    let mut e = Effect::new(actor, action, Outcome::Succeeded).after(details([
        ("registration", r.id.to_string().into()),
        ("type", r.kind.as_str().into()),
        ("name", r.name.clone().into()),
        ("version", r.version.clone().into()),
        ("sign_in", r.sign_in.into()),
    ]));
    e.workload = workload;
    e
}

/// Fails, in the transaction that withdrew their registrations, the calls sent to streams that
/// are gone: they fail as retryable rather than waiting out their deadline (§3.3 of
/// docs/16-extensions.md).
pub(super) async fn fail_in_flight(tx: &mut impl Tx, now: SystemTime) -> Result<(), AppError> {
    let ended = CallOutcome::Failed(CallFailure::stream_ended());
    tx.fail_calls_without_registration(&ended, now).await?;
    Ok(())
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Registers a stream the caller, an installed extension, opened on `replica`.
    pub async fn register_provider(
        &self,
        caller: &Caller,
        replica: Uuid,
        r: Register,
    ) -> Result<Registration, AppError> {
        let Principal::Extension(extension) = caller.principal else {
            return Err(AppError::Denied { action: "provider.register".into() });
        };
        let installation = self
            .store
            .extension(extension)
            .await?
            .filter(korpis_core::extension::Installation::is_installed)
            .ok_or(AppError::NotFound { kind: "extension" })?;
        checked(&r)?;
        let wanted = ProvidedInterface { kind: r.kind, name: r.name.clone() };
        if !self.store.provides_of(extension).await?.contains(&wanted) {
            let why = "one of the provides entries its installation recorded";
            return Err(AppError::invalid("register.name", why));
        }
        let now = self.clock.now();
        let registration = Registration {
            id: Uuid::now_v7(),
            extension,
            organization: installation.organization,
            kind: r.kind,
            name: r.name,
            version: r.version,
            concurrency: r.concurrency,
            sign_in: r.sign_in,
            replica,
            connected_at: now,
            seen_at: now,
        };
        let mut tx = self.store.begin().await?;
        tx.insert_registration(&registration).await?;
        let workload = Some(installation.workload);
        let e = effect(caller.principal.clone(), "provider.register", &registration, workload);
        tx.append_effect(&e).await?;
        tx.commit().await?;
        Ok(registration)
    }

    /// Withdraws a registration whose stream ended; nothing if it was withdrawn already.
    pub async fn withdraw_provider(&self, registration: &Registration) -> Result<(), AppError> {
        let workload = self.store.extension(registration.extension).await?.map(|i| i.workload);
        let mut tx = self.store.begin().await?;
        if tx.delete_registration(registration.id).await? {
            let actor = Principal::Extension(registration.extension);
            tx.append_effect(&effect(actor, "provider.withdraw", registration, workload)).await?;
            fail_in_flight(&mut tx, self.clock.now()).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Marks the registrations `replica` holds as seen; those that still exist, so the replica
    /// can close the streams of any withdrawn meanwhile (by an uninstall, or by expiry).
    pub async fn keep_registrations(&self, replica: Uuid) -> Result<Vec<Uuid>, AppError> {
        let mut tx = self.store.begin().await?;
        let alive = tx.touch_registrations(replica, self.clock.now()).await?;
        tx.commit().await?;
        Ok(alive)
    }

    /// Withdraws every registration not seen for `stale`: its replica is gone, and so is its
    /// stream. How many.
    pub async fn expire_registrations(&self, stale: Duration) -> Result<u64, AppError> {
        let before = self.clock.now().checked_sub(stale).unwrap_or(std::time::UNIX_EPOCH);
        let mut tx = self.store.begin().await?;
        let gone = tx.expire_registrations(before).await?;
        for r in &gone {
            let workload = tx.extension_within(r.extension).await?.map(|i| i.workload);
            let actor = Principal::System(SystemActor::Reconciler);
            tx.append_effect(&effect(actor, "provider.withdraw", r, workload)).await?;
        }
        if !gone.is_empty() {
            fail_in_flight(&mut tx, self.clock.now()).await?;
        }
        tx.commit().await?;
        Ok(u64::try_from(gone.len()).unwrap_or(u64::MAX))
    }
}
