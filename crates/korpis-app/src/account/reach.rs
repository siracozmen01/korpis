// SPDX-License-Identifier: AGPL-3.0-or-later

//! How far a session reaches, and what a confined one may change of its account (§9.8 of
//! docs/08-identity.md, Findings 131 and 133 of docs/23-walkthroughs.md). A session begun at a
//! portal reaches beneath the portal's organization; one begun with a password and a second
//! factor both set beneath nested organizations reaches beneath the narrower, wherever it is
//! begun, since one holder may have seen both; and one that is both reaches beneath the narrower
//! of all, a sign-in where they are disjoint being refused, since it would reach nothing.

use super::own_holder;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, GrantWrite, RecipeSource, Store};
use korpis_core::account::exposed::Shared;
use korpis_core::account::session::UserSession;
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::principal::Principal;
use std::collections::BTreeSet;
use uuid::Uuid;

/// The refusal of what a confined session may not do: to be done at the installation's own
/// address, by a session that reaches further.
pub(crate) fn confined() -> AppError {
    AppError::conflict(
        "session.portal_confined",
        "a session confined beneath an organization cannot do this: do it at the installation's \
         own address, signed in with a factor no portal has seen",
    )
}

/// Refuses registering a passkey through `session` unless it is confined exactly as its address
/// is: a passkey signs in reaching what its address reaches, and whoever holds the session holds
/// the authenticator it registers.
pub(super) fn registers_passkeys(session: &UserSession) -> Result<(), AppError> {
    if session.within == session.portal.map(|p| p.organization) { Ok(()) } else { Err(confined()) }
}

/// Whether a session confined as `own` sees `other`, a session of the same holder: one that
/// is not confined sees every one, and one that is sees those begun at its own portal and
/// confined as it is, the rest being where its holder answers for nothing.
pub(super) fn sees_session(own: Option<&UserSession>, other: &UserSession) -> bool {
    own.is_none_or(|own| {
        other.portal.map(|p| p.portal) == own.portal.map(|p| p.portal) && other.within == own.within
    })
}

/// Whether a session confined as `own` sees a passkey registered at `portal`: one that is
/// not confined sees every one, and one that is sees those of its own portal, and none at the
/// installation's own address.
pub(super) fn sees_passkey(own: Option<&UserSession>, portal: Option<Uuid>) -> bool {
    own.is_none_or(|own| own.portal.is_some_and(|at| Some(at.portal) == portal))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The caller's session, if it is confined: what bounds the sessions and passkeys it sees
    /// and ends (§9.8 of docs/08-identity.md); none for one that is not.
    pub(super) async fn confining(&self, caller: &Caller) -> Result<Option<UserSession>, AppError> {
        if caller.within.is_none() {
            return Ok(None);
        }
        let (holder, id) = (own_holder(caller)?, caller.session.ok_or(AppError::Unauthenticated)?);
        let session = self.store.user_session(holder, id).await?;
        session.map(Some).ok_or(AppError::Unauthenticated)
    }

    /// `a` if it lies at or beneath `b`, `b` if it lies beneath `a`, and none if neither does.
    async fn narrower(&self, a: Uuid, b: Uuid) -> Result<Option<Uuid>, AppError> {
        if a == b {
            return Ok(Some(a));
        }
        let org = |id| ObjectRef::new(Kind::Organization, id);
        if self.store.ancestry(org(a)).await?.iter().any(|n| n.object == org(b)) {
            return Ok(Some(a));
        }
        if self.store.ancestry(org(b)).await?.iter().any(|n| n.object == org(a)) {
            return Ok(Some(b));
        }
        Ok(None)
    }

    /// The organization a sign-in to `account` with its password and `second` reaches beneath,
    /// from where the two were set: the narrower, when both were set beneath organizations one
    /// of which lies beneath the other, and none otherwise, no one holder having seen both.
    pub(super) async fn seen_by(
        &self,
        account: Uuid,
        second: Shared,
    ) -> Result<Option<Uuid>, AppError> {
        let set = self.store.secrets_set_within(account).await?;
        match set.of_sign_in(second) {
            Some((password, other)) => self.narrower(password, other).await,
            None => Ok(None),
        }
    }

    /// How far a session begun at a portal of `at` with secrets seen beneath `seen` reaches:
    /// beneath the narrower of the two, or refused where neither lies beneath the other.
    pub(super) async fn reach(
        &self,
        at: Option<Uuid>,
        seen: Option<Uuid>,
    ) -> Result<Option<Uuid>, AppError> {
        match (at, seen) {
            (Some(a), Some(b)) => self.narrower(a, b).await?.map(Some).ok_or_else(confined),
            (a, b) => Ok(a.or(b)),
        }
    }

    /// Checks that a shared secret of `account` may be changed through a session confined
    /// beneath `within`: only while every grant the account holds lies beneath that
    /// organization, since the secret serves every address and the session's holder would
    /// otherwise replace what protects the rest (Finding 131). Read before the change's
    /// transaction, which [`still_changeable`] confirms against: the grants checked, none for a
    /// session that is not confined.
    pub(super) async fn secrets_changeable(
        &self,
        account: Uuid,
        within: Option<Uuid>,
    ) -> Result<Option<BTreeSet<Uuid>>, AppError> {
        let Some(org) = within else { return Ok(None) };
        let at = ObjectRef::new(Kind::Organization, org);
        let mut checked = BTreeSet::new();
        for chain in self.store.chains_held_by(&Principal::User(account).key()).await? {
            let Some(held) = chain.first() else { continue };
            let anchor = held.grant.scope.anchor();
            if !self.store.ancestry(anchor).await?.iter().any(|n| n.object == at) {
                return Err(confined());
            }
            checked.insert(held.grant.id);
        }
        Ok(Some(checked))
    }
}

/// Confirms in `tx` what [`App::secrets_changeable`] checked: every grant `account` holds now
/// is one it checked, so a grant issued meanwhile refuses the change rather than slipping past
/// it, and asking again decides afresh.
pub(super) async fn still_changeable<T: GrantWrite>(
    tx: &mut T,
    account: Uuid,
    checked: Option<&BTreeSet<Uuid>>,
) -> Result<(), AppError> {
    let Some(checked) = checked else { return Ok(()) };
    let held = tx.grants_held_now(&Principal::User(account).key()).await?;
    if held.iter().all(|g| checked.contains(&g.grant.id)) { Ok(()) } else { Err(confined()) }
}
