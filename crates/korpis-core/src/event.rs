// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an event subscription selects (§5 of docs/15-observability.md): kinds, a minimum
//! severity, and narrowing by project and workload labels. The log itself is the store's; this is
//! the part of it a subscriber writes, and that an extension's document declares.

use crate::labels::LabelSelector;
use crate::workload::observation::Health;
use crate::workload::status::Convergence;
use std::time::SystemTime;
use uuid::Uuid;

/// How much an event matters, in order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Something happened.
    #[default]
    Info,
    /// Something needs attention.
    Warning,
    /// Something is broken.
    Critical,
}

impl Severity {
    /// The name a document writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    /// The severity a document names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Info, Self::Warning, Self::Critical].into_iter().find(|v| v.as_str() == s)
    }
}

/// A condition the control plane decided changed: an event of its own, beside the effects
/// (§5 of docs/15-observability.md). Keyed to its workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    /// The workload it concerns.
    pub workload: Uuid,
    /// When it was decided, on the control plane's clock.
    pub at: SystemTime,
    /// What changed.
    pub change: ConditionChange,
}

/// What a condition event reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionChange {
    /// The application's health, as its node judged it.
    Health {
        /// Before.
        from: Health,
        /// After.
        to: Health,
    },
    /// How far the workload is from what was approved.
    Convergence {
        /// Before.
        from: Convergence,
        /// After.
        to: Convergence,
    },
}

impl ConditionChange {
    /// The event's kind.
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Health { .. } => "health.changed",
            Self::Convergence { .. } => "convergence.changed",
        }
    }

    /// The event's severity: a change for the worse is a warning.
    #[must_use]
    pub const fn severity(self) -> Severity {
        match self {
            Self::Health { to: Health::Unhealthy, .. }
            | Self::Convergence { to: Convergence::Failing, .. } => Severity::Warning,
            _ => Severity::Info,
        }
    }

    /// Before and after, by name.
    #[must_use]
    pub const fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::Health { from, to } => (from.as_str(), to.as_str()),
            Self::Convergence { from, to } => (from.as_str(), to.as_str()),
        }
    }
}

/// Which events a subscription receives, within what its subject may read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFilter {
    /// Kinds, each matched exactly or, ending in `.`, as a prefix. Empty matches every kind.
    pub kinds: Vec<String>,
    /// Events below it are not delivered.
    pub min_severity: Severity,
    /// Narrows to these projects; empty is every project the subject reaches.
    pub projects: Vec<Uuid>,
    /// Narrows to workloads whose labels match; an event about no workload passes it.
    pub workloads: LabelSelector,
}

/// Why a filter is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not an event kind: dotted lowercase words, a prefix ending in '.'")]
pub struct KindError(pub String);

/// Whether `pattern` is a kind or a kind prefix: dotted words of lowercase letters, digits, and
/// underscores, the last optionally empty to make a prefix (`workload.`).
fn well_formed(pattern: &str) -> bool {
    let body = pattern.strip_suffix('.').unwrap_or(pattern);
    !body.is_empty()
        && pattern.len() <= 128
        && body.split('.').all(|w| {
            !w.is_empty()
                && w.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

impl EventFilter {
    /// Checks every kind pattern.
    ///
    /// # Errors
    /// The first pattern that is not a kind or a kind prefix.
    pub fn validate(&self) -> Result<(), KindError> {
        match self.kinds.iter().find(|k| !well_formed(k)) {
            Some(bad) => Err(KindError(bad.clone())),
            None => Ok(()),
        }
    }

    /// Whether `event` passes the filter: its kind, its severity, and, when the filter narrows
    /// by them, its workload's project and labels. An event about no workload is about no
    /// project, so a filter naming projects passes none; a label selector narrows only events
    /// about workloads.
    #[must_use]
    pub fn matches(&self, event: &Event) -> bool {
        let projects =
            self.projects.is_empty() || event.project.is_some_and(|p| self.projects.contains(&p));
        let labels = event.workload.is_none() || self.workloads.matches(&event.labels);
        self.matches_kind(&event.kind) && event.severity >= self.min_severity && projects && labels
    }

    /// Whether an event of `kind` passes the kind patterns.
    #[must_use]
    pub fn matches_kind(&self, kind: &str) -> bool {
        self.kinds.is_empty()
            || self.kinds.iter().any(|k| match k.strip_suffix('.') {
                Some(prefix) => kind.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.')),
                None => kind == k,
            })
    }
}

/// An event as the log holds it (§5 of docs/15-observability.md).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Its position, which is its identifier.
    pub position: u64,
    /// What orders it: `workload:<id>` or `organization:<id>`.
    pub key: String,
    /// Its number within its key, from 1, without gaps.
    pub seq: u64,
    /// The ingest clock.
    pub at: SystemTime,
    /// An effect's action, or a condition.
    pub kind: String,
    /// How much it matters.
    pub severity: Severity,
    /// The organization it concerns.
    pub organization: Uuid,
    /// The workload it concerns, if one.
    pub workload: Option<Uuid>,
    /// That workload's project.
    pub project: Option<Uuid>,
    /// That workload's labels.
    pub labels: std::collections::BTreeMap<String, String>,
    /// The effect it reports, if it reports one.
    pub effect: Option<Uuid>,
    /// What happened.
    pub detail: serde_json::Map<String, serde_json::Value>,
}

/// A subscription: a stored position in the log, and what it selects (§5 of
/// docs/15-observability.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// Its identifier.
    pub id: Uuid,
    /// Who it delivers to, as a principal key; deliveries are judged by its grants.
    pub subject: crate::principal::Principal,
    /// What it selects.
    pub filter: EventFilter,
    /// The last position acknowledged; 0 before the first.
    pub acked: u64,
    /// When it was created.
    pub created: SystemTime,
    /// The installation that created it, which it ends with.
    pub extension: Option<Uuid>,
}

/// An event's identifier, which is its position: the position in the low 64 bits, so the
/// identifiers of later events are greater.
#[must_use]
pub const fn position_id(position: u64) -> Uuid {
    Uuid::from_u64_pair(0, position)
}

/// The position an event identifier names; `None` for one that names no position.
#[must_use]
pub const fn position_of(id: Uuid) -> Option<u64> {
    match id.as_u64_pair() {
        (0, position) if position > 0 => Some(position),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_match_exactly_or_by_prefix_and_severities_order() {
        let f = EventFilter {
            kinds: vec!["workload.".into(), "health.changed".into()],
            ..EventFilter::default()
        };
        assert!(f.validate().is_ok());
        assert!(f.matches_kind("workload.stop") && f.matches_kind("health.changed"));
        assert!(!f.matches_kind("workloads.stop") && !f.matches_kind("workload"));
        assert!(!f.matches_kind("health.changed.again"));
        assert!(EventFilter::default().matches_kind("anything.at.all"));
        for bad in ["", ".", "Workload.stop", "a..b", "a.b-c"] {
            let f = EventFilter { kinds: vec![bad.into()], ..EventFilter::default() };
            assert!(f.validate().is_err(), "{bad:?}");
        }
        assert!(Severity::Critical > Severity::Warning && Severity::Warning > Severity::Info);
        assert_eq!(Severity::parse("warning"), Some(Severity::Warning));
        assert_eq!(Severity::parse("loud"), None);
    }

    fn event(kind: &str, project: Option<Uuid>, labels: &[(&str, &str)]) -> Event {
        Event {
            position: 1,
            key: "k".into(),
            seq: 1,
            at: SystemTime::UNIX_EPOCH,
            kind: kind.into(),
            severity: Severity::Warning,
            organization: Uuid::nil(),
            workload: project.map(|_| Uuid::nil()),
            project,
            labels: labels.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            effect: None,
            detail: serde_json::Map::new(),
        }
    }

    #[test]
    fn a_filter_narrows_by_kind_severity_project_and_labels() {
        let games = Uuid::from_u128(7);
        let mut f = EventFilter { kinds: vec!["workload.".into()], ..EventFilter::default() };
        assert!(f.matches(&event("workload.stop", Some(games), &[])));
        assert!(!f.matches(&event("grant.issued", None, &[])));
        f.min_severity = Severity::Critical;
        assert!(!f.matches(&event("workload.stop", Some(games), &[])), "below the minimum");
        f.min_severity = Severity::Info;
        f.projects = vec![games];
        assert!(f.matches(&event("workload.stop", Some(games), &[])));
        assert!(!f.matches(&event("workload.stop", Some(Uuid::from_u128(8)), &[])));
        f.kinds.clear();
        assert!(!f.matches(&event("org.write", None, &[])), "about no project");
        f.projects.clear();
        f.workloads.match_labels.insert("tier".into(), "public".into());
        assert!(f.matches(&event("org.write", None, &[])), "about no workload");
        assert!(!f.matches(&event("workload.stop", Some(games), &[("tier", "internal")])));
        assert!(f.matches(&event("workload.stop", Some(games), &[("tier", "public")])));
    }

    #[test]
    fn an_event_identifier_is_its_position_and_orders_like_it() {
        assert_eq!(position_of(position_id(42)), Some(42));
        assert!(position_id(9) < position_id(10));
        assert_eq!(position_of(Uuid::now_v7()), None, "an ordinary identifier names none");
        assert_eq!(position_of(position_id(0)), None, "positions start at 1");
    }
}
