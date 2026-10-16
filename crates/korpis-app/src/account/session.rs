// SPDX-License-Identifier: AGPL-3.0-or-later

//! User sessions (§9.4 of docs/08-identity.md): begun by a sign-in, kept alive by exchanging the
//! refresh credential for access tokens, rotated on every exchange, and ended by idleness, by
//! age, by its user, or by a rotated credential presented again, which only a copy can do.

use super::{own_account, record};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, Presented, RecipeSource, Store, Tx, UserSessionWrite as _};
use korpis_core::account::session::{SessionKind, UserSession, user_agent};
use korpis_core::audit::failure::source;
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::access::{ACCESS_LIFETIME, AccessClaims, mint};
use korpis_core::authz::credential::{Secret, SecretKind};
use korpis_core::principal::Principal;
use std::net::IpAddr;
use std::time::SystemTime;
use uuid::Uuid;

/// Who a session is begun for: where its refresh credential will be held, and what the client
/// said of itself and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    /// Where the refresh credential will be held.
    pub kind: SessionKind,
    /// The user agent, as it said.
    pub user_agent: String,
    /// The address it came from.
    pub ip: Option<IpAddr>,
}

/// A session begun, and its refresh credential, answered once and kept only as its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Begun {
    /// The session.
    pub id: Uuid,
    /// Its refresh credential.
    pub refresh: Secret,
    /// When it ends whatever is done with it.
    pub expires_at: SystemTime,
}

/// A refresh credential exchanged.
#[derive(Debug)]
pub struct Exchanged {
    /// The account.
    pub account: Uuid,
    /// The session.
    pub session: Uuid,
    /// The access token, the bearer of the account's calls until it expires.
    pub access: String,
    /// When the access token expires.
    pub access_expires_at: SystemTime,
    /// The session's next refresh credential, which replaces the one exchanged.
    pub refresh: Secret,
    /// When the session ends whatever is done with it.
    pub expires_at: SystemTime,
}

/// A session as its user sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    /// The session.
    pub session: UserSession,
    /// Whether it is the one the caller is using.
    pub current: bool,
}

fn internal(e: &impl std::fmt::Display) -> AppError {
    AppError::Internal(e.to_string())
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Begins a session for `account` and `client` in `tx`, a factor having just been checked.
    pub(super) async fn begin_session(
        &self,
        tx: &mut S::Tx,
        account: Uuid,
        client: &Client,
    ) -> Result<Begun, AppError> {
        let now = self.clock.now();
        let session = UserSession {
            id: Uuid::now_v7(),
            account,
            kind: client.kind,
            created_at: now,
            last_used_at: now,
            authenticated_at: now,
            expires_at: now + self.session_limits.max,
            user_agent: user_agent(&client.user_agent),
            source: source(client.ip),
        };
        let refresh = Secret::generate_kind(SecretKind::Refresh).map_err(|e| internal(&e))?;
        tx.insert_user_session(&session, &refresh.digest()).await?;
        let more = [
            ("session", session.id.to_string().into()),
            ("kind", client.kind.as_str().into()),
            ("source", session.source.clone().into()),
        ];
        record(tx, &Principal::User(account), "session.begun", account, more).await?;
        Ok(Begun { id: session.id, refresh, expires_at: session.expires_at })
    }

    /// Exchanges the refresh credential `presented` for an access token and its successor.
    pub async fn exchange(&self, presented: &str) -> Result<Exchanged, AppError> {
        let secret = Secret::presented_as(SecretKind::Refresh, presented)
            .ok_or(AppError::Unauthenticated)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let (session, usable) = match tx.present_refresh(&secret.digest()).await? {
            Presented::Current { session, usable } => (session, usable),
            Presented::Rotated { session, account } => {
                reused(&mut tx, session, account).await?;
                tx.commit().await?;
                return Err(AppError::Unauthenticated);
            }
            Presented::Unknown => return Err(AppError::Unauthenticated),
        };
        if !usable || !session.live(now, self.session_limits.idle) {
            tx.end_user_session(session.account, session.id).await?;
            tx.commit().await?;
            return Err(AppError::Unauthenticated);
        }
        let next = Secret::generate_kind(SecretKind::Refresh).map_err(|e| internal(&e))?;
        tx.rotate_refresh(session.id, &secret.digest(), &next.digest(), now).await?;
        let access_expires_at = now + ACCESS_LIFETIME;
        let claims = AccessClaims {
            user: session.account,
            session: session.id,
            expires_at: access_expires_at,
        };
        let access = mint(&self.token_key, &claims);
        tx.commit().await?;
        Ok(Exchanged {
            account: session.account,
            session: session.id,
            access,
            access_expires_at,
            refresh: next,
            expires_at: session.expires_at,
        })
    }

    /// Ends the session whose refresh credential is `presented`.
    pub async fn sign_out(&self, presented: &str) -> Result<(), AppError> {
        let secret = Secret::presented_as(SecretKind::Refresh, presented)
            .ok_or(AppError::Unauthenticated)?;
        let mut tx = self.store.begin().await?;
        let Presented::Current { session, .. } = tx.present_refresh(&secret.digest()).await? else {
            return Err(AppError::Unauthenticated);
        };
        tx.end_user_session(session.account, session.id).await?;
        let more = [("session", session.id.to_string().into())];
        let actor = Principal::User(session.account);
        record(&mut tx, &actor, "session.ended", session.account, more).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The caller's sessions, newest first.
    pub async fn sessions(&self, caller: &Caller) -> Result<Vec<SessionView>, AppError> {
        let account = own_account(caller)?;
        let now = self.clock.now();
        let sessions = self.store.user_sessions(account).await?;
        Ok(sessions
            .into_iter()
            .filter(|s| s.live(now, self.session_limits.idle))
            .map(|session| SessionView { current: caller.session == Some(session.id), session })
            .collect())
    }

    /// Ends the caller's session `id`.
    pub async fn end_session(&self, caller: &Caller, id: Uuid) -> Result<(), AppError> {
        let account = own_account(caller)?;
        let mut tx = self.store.begin().await?;
        if !tx.end_user_session(account, id).await? {
            return Err(AppError::NotFound { kind: "session" });
        }
        let more = [("session", id.to_string().into())];
        record(&mut tx, &caller.principal, "session.ended", account, more).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Ends every session of the caller's but the one it is using; how many it ended.
    pub async fn end_other_sessions(&self, caller: &Caller) -> Result<u64, AppError> {
        let account = own_account(caller)?;
        let mut tx = self.store.begin().await?;
        let ended = tx.end_other_sessions(account, caller.session).await?;
        let more = [("ended", ended.into())];
        record(&mut tx, &caller.principal, "session.others_ended", account, more).await?;
        tx.commit().await?;
        Ok(ended)
    }
}

/// Ends `session` of `account`, a rotated refresh credential of it having been presented, which
/// only a copy can do, and records it as the anonymous presenter's.
async fn reused<T: Tx>(tx: &mut T, session: Uuid, account: Uuid) -> Result<(), AppError> {
    tx.end_user_session(account, session).await?;
    let effect =
        Effect::new(Principal::Anonymous, "session.reused", Outcome::Succeeded).after(details([
            ("account", account.to_string().into()),
            ("session", session.to_string().into()),
        ]));
    tx.append_effect(&effect).await?;
    Ok(())
}
