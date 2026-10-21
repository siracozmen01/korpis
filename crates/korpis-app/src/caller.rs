// SPDX-License-Identifier: AGPL-3.0-or-later

//! The authenticated caller of a use case.

use crate::error::AppError;
use korpis_core::audit::{Effect, Outcome};
use korpis_core::authz::ticket::TicketObject;
use korpis_core::authz::{Action, Kind, ObjectRef, Resource, Scope, Subject};
use korpis_core::principal::Principal;
use std::net::IpAddr;

/// Who is asking, and how they reached us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Who, in structured form; effects record this.
    pub principal: Principal,
    /// Who, as authorization compares it.
    pub subject: Subject,
    /// The sets an identity provider has just asserted they are in, for a person acting through
    /// a relay; the grants those sets hold are theirs for the call (§5.2 of docs/08-identity.md).
    pub memberships: Vec<Subject>,
    /// Where the request came from, for network conditions.
    pub source_ip: Option<IpAddr>,
    /// Whether the credential carried a second factor. API tokens never do; a signed-in user's
    /// access token always does, since no sign-in is one factor (§9.6 of docs/08-identity.md).
    pub mfa: bool,
    /// The session a signed-in user's access token was exchanged under.
    pub session: Option<uuid::Uuid>,
    /// Who holds that session: an account, or a person signed in through an identity provider
    /// (§9.7 of docs/08-identity.md).
    pub holder: Option<korpis_core::principal::Holder>,
    /// For a person acting through a relay: the ticket they act under.
    pub relayed: Option<Relayed>,
    /// For a session begun at a portal, the portal's organization: nothing outside it is
    /// authorized for the call, whatever its grants reach (§9.8 of docs/08-identity.md).
    pub within: Option<uuid::Uuid>,
}

/// A person acting through a client that relayed what they presented, under the ticket it was
/// handed (§5.2 of docs/08-identity.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relayed {
    /// The client, whose own credential the call carried.
    pub relay: Principal,
    /// The ticket it spent.
    pub ticket: uuid::Uuid,
    /// The one action the call may be authorized for.
    pub action: Action,
    /// The one object it may be authorized on.
    pub object: TicketObject,
}

impl Caller {
    /// A caller acting as `principal`.
    #[must_use]
    pub fn new(principal: Principal, source_ip: Option<IpAddr>, mfa: bool) -> Self {
        Self {
            subject: principal.subject(),
            principal,
            memberships: Vec::new(),
            source_ip,
            mfa,
            session: None,
            holder: None,
            relayed: None,
            within: None,
        }
    }

    /// The caller, acting under the user session `session` that `holder` holds.
    #[must_use]
    pub const fn with_session(
        mut self,
        holder: korpis_core::principal::Holder,
        session: uuid::Uuid,
    ) -> Self {
        self.session = Some(session);
        self.holder = Some(holder);
        self
    }

    /// An effect by this caller, naming the client that relayed them if one did, so the effect
    /// names everyone who took part (§5.2 of docs/08-identity.md).
    #[must_use]
    pub fn effect(&self, action: impl Into<String>, outcome: Outcome) -> Effect {
        let relay = self.relayed.as_ref().map(|r| r.relay.clone());
        Effect::new(self.principal.clone(), action, outcome).relayed_by(relay)
    }

    /// Refuses a person acting through a relay, for what no ticket can name: issuing a grant,
    /// minting a token, or anything else that would leave the relay holding what the person may
    /// do, or that acts outside the one authorization a ticket allows.
    ///
    /// # Errors
    /// [`AppError::Relayed`] for a relayed caller.
    pub const fn direct(&self) -> Result<(), AppError> {
        if self.relayed.is_some() { Err(AppError::Relayed) } else { Ok(()) }
    }

    /// Whether this caller may be authorized for `action` on `object`, whose ancestry is
    /// `resource`, at all: anyone may, but a person acting through a relay only for what their
    /// ticket names, and a session begun at a portal only at or beneath its organization.
    #[must_use]
    pub fn may_ask(&self, action: &Action, resource: &Resource) -> bool {
        let Some(object) = resource.object() else { return false };
        self.reaches(resource)
            && self
                .relayed
                .as_ref()
                .is_none_or(|r| &r.action == action && r.object == TicketObject::Object(object))
    }

    /// Whether `resource` lies where the caller may act at all: anywhere, but for a session
    /// begun at a portal only at or beneath the portal's organization (§9.8 of
    /// docs/08-identity.md).
    #[must_use]
    pub fn reaches(&self, resource: &Resource) -> bool {
        self.within.is_none_or(|org| {
            resource.is_within(&Scope::Object(ObjectRef::new(Kind::Organization, org)))
        })
    }

    /// Refuses `resource` if it lies outside where the caller may act ([`Self::reaches`]): what
    /// a session begun at a portal issues or approves is held beneath its organization, as
    /// every call it makes is.
    ///
    /// # Errors
    /// [`AppError::Conflict`] `session.portal_confined` for such a resource.
    pub fn confined(&self, resource: Option<&Resource>) -> Result<(), AppError> {
        if self.within.is_none() || resource.is_some_and(|r| self.reaches(r)) {
            return Ok(());
        }
        Err(AppError::conflict(
            "session.portal_confined",
            "a session begun at a portal acts only beneath the portal's organization: sign in at \
             the installation's own address for this",
        ))
    }

    /// Refuses a person acting through a relay whose ticket does not name revoking `grant`.
    ///
    /// # Errors
    /// [`AppError::Relayed`] for such a ticket.
    pub fn may_revoke(&self, grant: uuid::Uuid) -> Result<(), AppError> {
        match &self.relayed {
            Some(r)
                if r.action.as_str() != "grant.revoke"
                    || r.object != TicketObject::Grant(grant) =>
            {
                Err(AppError::Relayed)
            }
            _ => Ok(()),
        }
    }

    /// This caller with its ticket's grant standing for `anchor`, the object `grant.revoke`
    /// over the grant's scope is authorized on (§3.5 of docs/08-identity.md), once
    /// [`Self::may_revoke`] has passed.
    #[must_use]
    pub fn revoking_at(&self, anchor: ObjectRef) -> Self {
        let mut narrowed = self.clone();
        if let Some(r) = &mut narrowed.relayed {
            r.object = TicketObject::Object(anchor);
        }
        narrowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_core::authz::Node;
    use uuid::Uuid;

    /// `object`, beneath each of `above`, nearest first.
    fn beneath(object: ObjectRef, above: &[ObjectRef]) -> Resource {
        let node = |object| Node {
            object,
            labels: std::collections::BTreeMap::new(),
            suspended: false,
            ceiling: None,
        };
        Resource { chain: std::iter::once(object).chain(above.iter().copied()).map(node).collect() }
    }

    #[test]
    fn a_session_begun_at_a_portal_reaches_only_beneath_its_organization() {
        let (root, reseller) = (Uuid::now_v7(), Uuid::now_v7());
        let (root, reseller) = (
            ObjectRef::new(Kind::Organization, root),
            ObjectRef::new(Kind::Organization, reseller),
        );
        let inside = beneath(ObjectRef::new(Kind::Project, Uuid::now_v7()), &[reseller, root]);
        let outside = beneath(ObjectRef::new(Kind::Project, Uuid::now_v7()), &[root]);
        let mut caller = Caller::new(Principal::User(Uuid::now_v7()), None, true);
        assert!(caller.reaches(&outside), "a session begun at the installation's own address");
        caller.within = Some(reseller.id);
        assert!(caller.reaches(&inside));
        assert!(caller.reaches(&beneath(reseller, &[root])), "the organization itself");
        assert!(!caller.reaches(&outside));
        assert!(!caller.reaches(&beneath(root, &[])));
        let read = Action::parse("org.read").unwrap();
        assert!(!caller.may_ask(&read, &outside));
        assert!(caller.confined(Some(&inside)).is_ok());
        assert!(caller.confined(Some(&outside)).is_err());
        assert!(caller.confined(None).is_err(), "nothing known of it is nothing reached");
    }
}
