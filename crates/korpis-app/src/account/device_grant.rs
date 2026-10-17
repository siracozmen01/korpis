// SPDX-License-Identifier: AGPL-3.0-or-later

//! The CLI's sign-in (§9.4 of docs/08-identity.md): the OAuth device authorization grant of RFC
//! 8628. A CLI asks and is given a device code to poll with and a user code for its user; an
//! account signed in to the panel, recently, approves the user code there, having been shown where
//! the CLI asked from; and the CLI's next poll begins a session of its own and spends the grant.

use super::{Begun, Client, own_account, record};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, DeviceGrantWrite as _, Polled, RecipeSource, Store, Tx as _};
use korpis_core::account::device_grant::{
    DEVICE_GRANT_LIFETIME, DeviceAuthorization, POLL_INTERVAL, SLOW_DOWN, new_user_code, user_code,
};
use korpis_core::account::session::{SessionKind, user_agent};
use korpis_core::audit::failure::source;
use korpis_core::authz::credential::{Secret, SecretKind};
use korpis_core::principal::Principal;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How many user codes are drawn before giving up: each is taken only while another pending
/// authorization holds it, which 20^8 codes make all but impossible.
const DRAWS: usize = 4;

/// A device authorization begun: what the CLI keeps and what it shows its user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorized {
    /// The device code, which the CLI polls with and shows no one.
    pub device_code: Secret,
    /// The user code, as it is shown: `BCDF-GHJK`.
    pub user_code: String,
    /// When it can no longer be approved.
    pub expires_at: SystemTime,
    /// How long to wait between polls.
    pub interval: Duration,
}

/// A device authorization approved, and the session the poll that found it began.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSignedIn {
    /// The account that approved it.
    pub account: Uuid,
    /// The CLI's session, and its refresh credential.
    pub session: Begun,
}

fn internal(e: &impl std::fmt::Display) -> AppError {
    AppError::Internal(e.to_string())
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Begins a device authorization for the CLI `client`.
    pub async fn authorize_device(&self, client: &Client) -> Result<Authorized, AppError> {
        let device_code =
            Secret::generate_kind(SecretKind::DeviceCode).map_err(|e| internal(&e))?;
        let now = self.clock.now();
        for _ in 0..DRAWS {
            let a = DeviceAuthorization {
                id: Uuid::now_v7(),
                user_code: new_user_code().map_err(|e| internal(&e))?,
                created_at: now,
                expires_at: now + DEVICE_GRANT_LIFETIME,
                interval: POLL_INTERVAL,
                user_agent: user_agent(&client.user_agent),
                source: source(client.ip),
                decision: None,
            };
            let mut tx = self.store.begin().await?;
            if tx.insert_device_authorization(&a, &device_code.digest()).await? {
                tx.commit().await?;
                let user_code = a.shown();
                return Ok(Authorized {
                    device_code,
                    user_code,
                    expires_at: a.expires_at,
                    interval: a.interval,
                });
            }
        }
        Err(internal(&"no free user code"))
    }

    /// Polls with `device_code` from `client`: the CLI's session once its authorization is
    /// approved, and until then why not.
    pub async fn device_token(
        &self,
        device_code: &str,
        client: &Client,
    ) -> Result<DeviceSignedIn, AppError> {
        let secret = Secret::presented_as(SecretKind::DeviceCode, device_code)
            .ok_or(AppError::Unauthenticated)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let Polled::Found { authorization: a, polled_before, honoured } =
            tx.poll_device(&secret.digest(), now).await?
        else {
            return Err(AppError::Unauthenticated);
        };
        if !a.live(now) {
            tx.delete_device_authorization(a.id).await?;
            tx.commit().await?;
            return Err(AppError::conflict(
                "device.expired",
                "the authorization expired; sign in again",
            ));
        }
        if polled_before.is_some_and(|before| now < before + a.interval) {
            let slower = a.interval + SLOW_DOWN;
            tx.slow_device(a.id, u32::try_from(slower.as_secs()).unwrap_or(u32::MAX)).await?;
            tx.commit().await?;
            return Err(AppError::RateLimited { retry_after: slower, standing: Vec::new() });
        }
        let Some(decision) = a.decision else {
            tx.commit().await?;
            return Err(AppError::conflict("device.pending", "not approved yet; poll again"));
        };
        tx.delete_device_authorization(a.id).await?;
        if !decision.approved {
            tx.commit().await?;
            return Err(AppError::conflict("device.denied", "the authorization was denied"));
        }
        if !honoured {
            tx.commit().await?;
            return Err(AppError::conflict(
                "device.denied",
                "the approval was withdrawn: the session that gave it has ended, or its account \
                 may no longer sign in",
            ));
        }
        let cli = Client { kind: SessionKind::Cli, ..client.clone() };
        let session = self.begin_session(&mut tx, decision.account, &cli).await?;
        tx.commit().await?;
        Ok(DeviceSignedIn { account: decision.account, session })
    }

    /// The pending device authorization whose user code is `typed`, as the caller is shown it
    /// before deciding.
    pub async fn device_authorization(
        &self,
        caller: &Caller,
        typed: &str,
    ) -> Result<DeviceAuthorization, AppError> {
        own_account(caller)?;
        let code = user_code(typed).ok_or(AppError::NotFound { kind: "device authorization" })?;
        let now = self.clock.now();
        self.store
            .device_authorization(&code)
            .await?
            .filter(|a| a.live(now) && a.decision.is_none())
            .ok_or(AppError::NotFound { kind: "device authorization" })
    }

    /// Approves, or denies, the pending device authorization whose user code is `typed`, for
    /// the caller's account. Approving begins a session for whoever holds the device code, so
    /// it needs a recent sign-in, as a change of factors does, and is honoured only while the
    /// session it was given from lasts and its account may sign in (Findings 74 and 75 of
    /// docs/23-walkthroughs.md); denying needs none.
    pub async fn decide_device(
        &self,
        caller: &Caller,
        typed: &str,
        approved: bool,
    ) -> Result<(), AppError> {
        let account =
            if approved { self.recently_signed_in(caller).await? } else { own_account(caller)? };
        let a = self.device_authorization(caller, typed).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let from = if approved { caller.session } else { None };
        if !tx.decide_device(&a.user_code, (account, from), approved, now).await? {
            return Err(AppError::NotFound { kind: "device authorization" });
        }
        let effect = if approved { "device.approved" } else { "device.denied" };
        let more = [("user_code", a.shown().into()), ("source", a.source.clone().into())];
        record(&mut tx, &Principal::User(account), effect, account, more).await?;
        tx.commit().await?;
        Ok(())
    }
}
