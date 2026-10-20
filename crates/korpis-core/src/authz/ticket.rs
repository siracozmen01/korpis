// SPDX-License-Identifier: AGPL-3.0-or-later

//! Acting as an external identity (§5.2 of docs/08-identity.md). A client that relays what a
//! person presented, a chat interaction, has the person's identity provider assert it, and is
//! handed a ticket: one action, on one object, once, within a minute, by that client alone. The
//! client never holds the person's authority; the gateway evaluates the person's own grants, and
//! those of the sets the assertion names, for that one call.

use super::action::Action;
use super::binding::ProviderRef;
use crate::object::ObjectRef;
use crate::principal::{Principal, SetName};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long a ticket may wait to be used: one interaction's command, not a session.
pub const TICKET_LIFETIME: Duration = Duration::from_secs(60);
/// How long a presentation is remembered, so it is accepted once: a Discord interaction's own
/// lifetime, fifteen minutes.
pub const PRESENTATION_KEPT: Duration = Duration::from_secs(15 * 60);
/// How far an assertion's instant may be from the control plane's clock.
pub const ASSERTION_SKEW: Duration = Duration::from_secs(5 * 60);

// A presentation asserted at the edge of the skew ahead of the clock is believed until the skew
// has passed behind it, so it is remembered for longer than both, or it could be accepted twice.
const _: () = assert!(PRESENTATION_KEPT.as_secs() > 2 * ASSERTION_SKEW.as_secs());

/// One action, on one object, once, as an asserted external identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    /// Its identity.
    pub id: Uuid,
    /// The client that relayed the presentation, the only one that may use the ticket.
    pub relay: Principal,
    /// The person, as the provider asserted them.
    pub subject: Principal,
    /// The provider's sets the assertion says they are in.
    pub sets: Vec<SetName>,
    /// The provider that vouched.
    pub provider: ProviderRef,
    /// The presentation it was asserted from.
    pub presentation: String,
    /// The one action it permits asking for.
    pub action: Action,
    /// The one object it permits asking about.
    pub object: TicketObject,
    /// When it stops working, used or not.
    pub expires_at: SystemTime,
}

/// What a ticket's one action is asked of: an object, or, for `grant.revoke` alone, the grant to
/// revoke, so a person can kill a leaked link from where it leaked (§3 of
/// docs/12-surface-discord.md) and the ticket still names exactly one thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketObject {
    /// An object of the hierarchy.
    Object(ObjectRef),
    /// The grant `grant.revoke` revokes.
    Grant(Uuid),
}

impl TicketObject {
    /// Its kind, as kept: an object's kind, or `grant`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Object(o) => o.kind.as_str(),
            Self::Grant(_) => "grant",
        }
    }

    /// Its identifier.
    #[must_use]
    pub const fn id(&self) -> Uuid {
        match self {
            Self::Object(o) => o.id,
            Self::Grant(g) => *g,
        }
    }

    /// Whether `action` may be asked of it: a grant only for `grant.revoke`.
    #[must_use]
    pub fn admits(&self, action: &Action) -> bool {
        matches!(self, Self::Object(_)) || action.as_str() == "grant.revoke"
    }
}

/// Whether an assertion made at `at` may be believed at `now`.
#[must_use]
pub fn fresh(at: SystemTime, now: SystemTime) -> bool {
    let apart = now.duration_since(at).unwrap_or_else(|e| e.duration());
    apart <= ASSERTION_SKEW
}

/// The sets a ticket's holder is evaluated as a member of, beside the person themself.
#[must_use]
pub fn sets_of(ticket: &Ticket) -> Vec<Principal> {
    let provider = ticket.provider.key();
    ticket.sets.iter().map(|s| s.at(&provider)).collect()
}
