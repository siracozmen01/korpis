// SPDX-License-Identifier: AGPL-3.0-or-later

//! A person signed in through an identity provider, calling (§9.7 of docs/08-identity.md). Their
//! access token names their session, which the gateway reads at each call: it must not have
//! ended, its holder's binding must not be retired, and of the sets the provider named at the
//! sign-in, the caller holds only those whose record of the person as a member still vouches,
//! at the control plane's clock. No second factor is theirs (§9), so a grant requiring one does
//! not permit them.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::account::session::UserSession;
use korpis_core::authz::Subject;
use korpis_core::authz::access::AccessClaims;
use korpis_core::authz::binding::IdentityBinding;
use korpis_core::principal::Principal;
use std::net::IpAddr;
use std::time::SystemTime;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The caller a person's access token `claims` stands for, or unauthenticated.
    pub(super) async fn person(
        &self,
        claims: &AccessClaims,
        source_ip: Option<IpAddr>,
        now: SystemTime,
    ) -> Result<Caller, AppError> {
        let binding = claims.holder.binding().ok_or(AppError::Unauthenticated)?;
        let session = self.store.user_session(claims.holder, claims.session).await?;
        let session = session
            .filter(|s| s.live(now, self.session_limits.idle))
            .ok_or(AppError::Unauthenticated)?;
        let person = self.store.binding(binding).await?;
        let person = person.filter(|b| b.retired_at.is_none()).ok_or(AppError::Unauthenticated)?;
        let memberships = self.vouched_sets(&person, &session, now).await?;
        let mut caller = Caller::new(person.subject.clone(), source_ip, false)
            .with_session(claims.holder, claims.session);
        caller.memberships = memberships;
        Ok(caller)
    }

    /// The sets of `session` whose record of `person` as a member vouches at `now`: a set with
    /// no live binding, or that has no such record, or whose record has lapsed, is left out.
    async fn vouched_sets(
        &self,
        person: &IdentityBinding,
        session: &UserSession,
        now: SystemTime,
    ) -> Result<Vec<Subject>, AppError> {
        let member = person.subject.key();
        let mut held = Vec::new();
        for s in &session.sets {
            let set = Principal::ExternalSet {
                provider: person.provider.key(),
                realm: s.realm.clone(),
                member_of: s.member_of.clone(),
            };
            let Some(bound) = self.store.binding_of(&set.key()).await? else { continue };
            let record = self.store.member(bound.id, &member).await?;
            if record.and_then(|r| r.verified_until(&bound)).is_some_and(|until| now <= until) {
                held.push(set.subject());
            }
        }
        Ok(held)
    }
}
