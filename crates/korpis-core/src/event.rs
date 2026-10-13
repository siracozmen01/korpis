// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an event subscription selects (§5 of docs/15-observability.md): kinds, a minimum
//! severity, and narrowing by project and workload labels. The log itself is the store's; this is
//! the part of it a subscriber writes, and that an extension's document declares.

use crate::labels::LabelSelector;
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
}
