// SPDX-License-Identifier: AGPL-3.0-or-later

//! Searching a stream within its retention window (§8 of docs/14-streams.md): a substring, or a
//! regular expression, matched against each text or structured record's body. It is a property
//! of the segment files rather than a search product: no index, a scan of what the node holds.
//!
//! A pattern comes from whoever holds `workload.console.read`, and the node runs it, so it is
//! bounded. Regular expressions are matched by an engine with no backtracking, in time linear in
//! the input whatever the pattern, and a pattern whose compiled form would be large is refused.

use crate::record::{Kind, Record};
use regex::bytes::{Regex, RegexBuilder};

/// The longest pattern accepted, in bytes.
pub const MAX_PATTERN: usize = 1024;

/// The most memory a compiled regular expression may take.
const COMPILED_LIMIT: usize = 1 << 20;

/// Why a pattern was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    /// Longer than [`MAX_PATTERN`].
    #[error("a pattern holds at most {MAX_PATTERN} bytes")]
    TooLong,
    /// Not a regular expression, or one too large to run.
    #[error("not a usable regular expression: {0}")]
    Invalid(String),
}

/// Which records a search keeps.
#[derive(Debug, Clone)]
pub enum Matcher {
    /// Every record.
    All,
    /// Records whose body contains these bytes.
    Substring(Vec<u8>),
    /// Records whose body matches.
    Regex(Regex),
}

impl Matcher {
    /// The matcher for `pattern`, a regular expression if `regex`; every record if it is empty.
    pub fn new(pattern: &str, regex: bool) -> Result<Self, PatternError> {
        if pattern.is_empty() {
            return Ok(Self::All);
        }
        if pattern.len() > MAX_PATTERN {
            return Err(PatternError::TooLong);
        }
        if !regex {
            return Ok(Self::Substring(pattern.as_bytes().to_vec()));
        }
        RegexBuilder::new(pattern)
            .size_limit(COMPILED_LIMIT)
            .dfa_size_limit(COMPILED_LIMIT)
            .build()
            .map(Self::Regex)
            .map_err(|e| PatternError::Invalid(e.to_string()))
    }

    /// Whether `record` is kept. A gap or control record always is: a search must say where it
    /// could not look, and a terminal event is part of what the reader sees.
    #[must_use]
    pub fn keeps(&self, record: &Record) -> bool {
        if matches!(record.kind, Kind::Gap | Kind::Control) {
            return true;
        }
        match self {
            Self::All => true,
            Self::Substring(needle) => record.body.windows(needle.len()).any(|w| w == needle),
            Self::Regex(re) => re.is_match(&record.body),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Source;

    fn record(kind: Kind, body: &str) -> Record {
        Record { offset: 0, time_nanos: 0, source: Source::Stdout, kind, body: body.into() }
    }

    #[test]
    fn substrings_and_regexes_keep_what_matches_and_every_gap() {
        let joined = record(Kind::Text, "Player Alex joined the game");
        let saved = record(Kind::Text, "Saved the game");
        let gap = record(Kind::Gap, r#"{"records":3}"#);
        let alex = Matcher::new("Alex", false).unwrap();
        assert!(alex.keeps(&joined) && !alex.keeps(&saved) && alex.keeps(&gap));
        let re = Matcher::new(r"^Player \w+ joined", true).unwrap();
        assert!(re.keeps(&joined) && !re.keeps(&saved));
        assert!(Matcher::new("", true).unwrap().keeps(&saved), "empty is everything");
    }

    #[test]
    fn a_hostile_pattern_is_refused_or_runs_in_linear_time() {
        assert_eq!(
            Matcher::new(&"a".repeat(MAX_PATTERN + 1), false).unwrap_err(),
            PatternError::TooLong
        );
        assert!(matches!(Matcher::new("(unclosed", true), Err(PatternError::Invalid(_))));
        assert!(matches!(Matcher::new(r"\w{1000}{1000}", true), Err(PatternError::Invalid(_))));
        // The pattern that makes a backtracking engine take exponential time.
        let evil = Matcher::new("(a+)+$", true).unwrap();
        let started = std::time::Instant::now();
        assert!(!evil.keeps(&record(Kind::Text, &format!("{}!", "a".repeat(10_000)))));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
}
