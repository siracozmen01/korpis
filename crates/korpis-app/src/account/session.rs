// SPDX-License-Identifier: AGPL-3.0-or-later

//! User sessions (§9.4 of docs/08-identity.md): begun by a sign-in, kept alive by exchanging the
//! refresh credential for access tokens, rotated on every exchange, and ended by idleness, by
//! age, by its user, or by a rotated credential presented again, which only a copy can do.

use super::{Device, own_holder, record_held};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, Presented, RecipeSource, Store, Tx, UserSessionWrite as _};
use korpis_core::account::session::{AtPortal, Holder, SessionKind, UserSession, user_agent};
use korpis_core::audit::failure::source;
use korpis_core::authz::access::{ACCESS_LIFETIME, AccessClaims, mint};
use korpis_core::authz::credential::{Secret, SecretKind};
use korpis_core::principal::{Principal, SetName};
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
    /// The device cookies it presented (§9.6 of docs/08-identity.md), which only a password
    /// sign-in asks about.
    pub devices: Vec<String>,
    /// The portal it reached the control plane at, whose connection named it (§9.8 of
    /// docs/08-identity.md); none at the installation's own address.
    pub portal: Option<AtPortal>,
}

impl Client {
    /// The portal its ceremonies are held at, none at the installation's own address.
    #[must_use]
    pub fn address(&self) -> Option<Uuid> {
        self.portal.map(|p| p.portal)
    }
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
    /// The device cookie a browser is given for the account (§9.6 of docs/08-identity.md);
    /// none for the CLI, which has no browser to keep one.
    pub device: Option<Device>,
}

/// A refresh credential exchanged.
#[derive(Debug)]
pub struct Exchanged {
    /// Who holds the session.
    pub holder: Holder,
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
        let held = (Holder::Account(account), Principal::User(account));
        self.begin_held(tx, held, Vec::new(), client).await
    }

    /// Begins a session `holder` holds, acting as `actor`, with `sets` for a person, for
    /// `client` in `tx`: what vouched for the holder having just been checked. The actor is
    /// given rather than read, since a person's binding may be made in `tx` itself.
    pub(crate) async fn begin_held(
        &self,
        tx: &mut S::Tx,
        (holder, actor): (Holder, Principal),
        sets: Vec<SetName>,
        client: &Client,
    ) -> Result<Begun, AppError> {
        let now = self.clock.now();
        let session = UserSession {
            id: Uuid::now_v7(),
            holder,
            sets,
            kind: client.kind,
            created_at: now,
            last_used_at: now,
            authenticated_at: now,
            expires_at: now + self.session_limits.max,
            user_agent: user_agent(&client.user_agent),
            source: source(client.ip),
            portal: client.portal,
        };
        let refresh = Secret::generate_kind(SecretKind::Refresh).map_err(|e| internal(&e))?;
        tx.insert_user_session(&session, &refresh.digest()).await?;
        let more = [
            ("session", session.id.to_string().into()),
            ("kind", client.kind.as_str().into()),
            ("source", session.source.clone().into()),
        ];
        record_held(tx, &actor, "session.begun", holder, more).await?;
        // A device cookie spares an account's browsers its password guesses (§9.6); a person
        // signed in through a provider has no password here to guess.
        let device = holder
            .account()
            .filter(|_| client.kind == SessionKind::Web)
            .map(|account| self.device_for(account));
        Ok(Begun { id: session.id, refresh, expires_at: session.expires_at, device })
    }

    /// Whom a session `holder` holds acts as: the account, or the person their binding binds.
    pub(crate) async fn actor_of(&self, holder: Holder) -> Result<Principal, AppError> {
        match holder {
            Holder::Account(id) => Ok(Principal::User(id)),
            Holder::Person(binding) => {
                let bound = self.store.binding(binding).await?;
                bound.map(|b| b.subject).ok_or_else(|| internal(&"a session of no binding"))
            }
        }
    }

    /// Exchanges the refresh credential `presented` for an access token and its successor.
    pub async fn exchange(&self, presented: &str) -> Result<Exchanged, AppError> {
        let secret = Secret::presented_as(SecretKind::Refresh, presented)
            .ok_or(AppError::Unauthenticated)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let (session, usable) = match tx.present_refresh(&secret.digest()).await? {
            Presented::Current { session, usable } => (session, usable),
            Presented::Rotated { session, holder } => {
                reused(&mut tx, session, holder).await?;
                tx.commit().await?;
                return Err(AppError::Unauthenticated);
            }
            Presented::Unknown => return Err(AppError::Unauthenticated),
        };
        if !usable || !session.live(now, self.session_limits.idle) {
            tx.end_user_session(session.holder, session.id).await?;
            tx.commit().await?;
            return Err(AppError::Unauthenticated);
        }
        let next = Secret::generate_kind(SecretKind::Refresh).map_err(|e| internal(&e))?;
        tx.rotate_refresh(session.id, &secret.digest(), &next.digest(), now).await?;
        let access_expires_at = now + ACCESS_LIFETIME;
        let claims = AccessClaims {
            holder: session.holder,
            session: session.id,
            within: session.portal.map(|p| p.organization),
            expires_at: access_expires_at,
        };
        let access = mint(&self.token_key, &claims);
        tx.commit().await?;
        Ok(Exchanged {
            holder: session.holder,
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
        tx.end_user_session(session.holder, session.id).await?;
        let more = [("session", session.id.to_string().into())];
        let actor = self.actor_of(session.holder).await?;
        record_held(&mut tx, &actor, "session.ended", session.holder, more).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The caller's sessions, newest first.
    pub async fn sessions(&self, caller: &Caller) -> Result<Vec<SessionView>, AppError> {
        let holder = own_holder(caller)?;
        let now = self.clock.now();
        let sessions = self.store.user_sessions(holder).await?;
        Ok(sessions
            .into_iter()
            .filter(|s| s.live(now, self.session_limits.idle))
            .map(|session| SessionView { current: caller.session == Some(session.id), session })
            .collect())
    }

    /// Ends the caller's session `id`.
    pub async fn end_session(&self, caller: &Caller, id: Uuid) -> Result<(), AppError> {
        let holder = own_holder(caller)?;
        let mut tx = self.store.begin().await?;
        if !tx.end_user_session(holder, id).await? {
            return Err(AppError::NotFound { kind: "session" });
        }
        let more = [("session", id.to_string().into())];
        record_held(&mut tx, &caller.principal, "session.ended", holder, more).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Ends every session of the caller's but the one it is using; how many it ended.
    pub async fn end_other_sessions(&self, caller: &Caller) -> Result<u64, AppError> {
        let holder = own_holder(caller)?;
        let mut tx = self.store.begin().await?;
        let ended = tx.end_other_sessions(holder, caller.session).await?;
        let more = [("ended", ended.into())];
        record_held(&mut tx, &caller.principal, "session.others_ended", holder, more).await?;
        tx.commit().await?;
        Ok(ended)
    }
}

/// Ends `session` of `holder`, a rotated refresh credential of it having been presented, which
/// only a copy can do, and records it as the anonymous presenter's.
async fn reused<T: Tx>(tx: &mut T, session: Uuid, holder: Holder) -> Result<(), AppError> {
    tx.end_user_session(holder, session).await?;
    let more = [("session", session.to_string().into())];
    record_held(tx, &Principal::Anonymous, "session.reused", holder, more).await
}
