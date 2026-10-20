// SPDX-License-Identifier: AGPL-3.0-or-later

//! The authenticated caller of a use case.

use crate::error::AppError;
use korpis_core::audit::{Effect, Outcome};
use korpis_core::authz::ticket::TicketObject;
use korpis_core::authz::{Action, ObjectRef, Subject};
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

    /// Whether this caller may be authorized for `action` on `object` at all: anyone may, but a
    /// person acting through a relay only for what their ticket names.
    #[must_use]
    pub fn may_ask(&self, action: &Action, object: ObjectRef) -> bool {
        self.relayed
            .as_ref()
            .is_none_or(|r| &r.action == action && r.object == TicketObject::Object(object))
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
