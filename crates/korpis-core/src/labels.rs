// SPDX-License-Identifier: AGPL-3.0-or-later

//! Labels: the key-value pairs selectors match (§3 of docs/01-model.md).
//!
//! The syntax is Kubernetes', deliberately: operators already know it, and a selector written for
//! one system reads the same in the other. A key is an optional DNS-subdomain prefix and a `/`,
//! then a name of at most 63 characters; a value is empty or a name.

use std::collections::BTreeMap;

/// Validated labels, ordered by key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Labels(BTreeMap<String, String>);

/// Why a label is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LabelError {
    /// The key is malformed.
    #[error("label key {0:?}: an optional DNS prefix and '/', then 1 to 63 of [A-Za-z0-9-_.]")]
    Key(String),
    /// The value is malformed.
    #[error("label value {0:?}: at most 63 of [A-Za-z0-9-_.], beginning and ending alphanumeric")]
    Value(String),
}

const MAX_NAME: usize = 63;
const MAX_PREFIX: usize = 253;

fn is_name(s: &str) -> bool {
    let bytes = s.as_bytes();
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else { return false };
    s.len() <= MAX_NAME
        && first.is_ascii_alphanumeric()
        && last.is_ascii_alphanumeric()
        && bytes.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn is_dns_subdomain(s: &str) -> bool {
    s.len() <= MAX_PREFIX
        && s.split('.').all(|l| {
            let b = l.as_bytes();
            !b.is_empty()
                && b.len() <= MAX_NAME
                && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
                && b.first().is_some_and(u8::is_ascii_alphanumeric)
                && b.last().is_some_and(u8::is_ascii_alphanumeric)
        })
}

fn is_key(key: &str) -> bool {
    match key.split_once('/') {
        Some((prefix, name)) => is_dns_subdomain(prefix) && is_name(name),
        None => is_name(key),
    }
}

impl Labels {
    /// Validates every pair.
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Result<Self, LabelError> {
        let mut map = BTreeMap::new();
        for (k, v) in pairs {
            if !is_key(&k) {
                return Err(LabelError::Key(k));
            }
            if !v.is_empty() && !is_name(&v) {
                return Err(LabelError::Value(v));
            }
            map.insert(k, v);
        }
        Ok(Self(map))
    }

    /// The pairs, ordered by key.
    #[must_use]
    pub fn as_map(&self) -> &BTreeMap<String, String> {
        &self.0
    }

    /// The pairs, by value.
    #[must_use]
    pub fn into_map(self) -> BTreeMap<String, String> {
        self.0
    }
}

/// A label selector: every `match_labels` pair must be present, and every expression must hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelSelector {
    /// Labels that must be present with these values.
    pub match_labels: BTreeMap<String, String>,
    /// Further expressions, all of which must hold.
    pub expressions: Vec<LabelExpression>,
}

/// One label expression of a selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelExpression {
    /// The label is present with one of the values.
    In(String, Vec<String>),
    /// The label is absent or has none of the values.
    NotIn(String, Vec<String>),
    /// The label is present.
    Exists(String),
    /// The label is absent.
    DoesNotExist(String),
}

impl LabelSelector {
    /// Whether `labels` satisfy the selector.
    #[must_use]
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.match_labels.iter().all(|(k, v)| labels.get(k) == Some(v))
            && self.expressions.iter().all(|e| match e {
                LabelExpression::In(k, vs) => labels.get(k).is_some_and(|v| vs.contains(v)),
                LabelExpression::NotIn(k, vs) => labels.get(k).is_none_or(|v| !vs.contains(v)),
                LabelExpression::Exists(k) => labels.contains_key(k),
                LabelExpression::DoesNotExist(k) => !labels.contains_key(k),
            })
    }

    /// Whether every object this selector matches is also matched by `wider`: it carries all of
    /// `wider`'s requirements, and possibly more. Conservative: a narrower selector written
    /// differently is refused rather than reasoned about.
    #[must_use]
    pub fn is_at_least_as_narrow_as(&self, wider: &Self) -> bool {
        wider.match_labels.iter().all(|(k, v)| self.match_labels.get(k) == Some(v))
            && wider.expressions.iter().all(|e| self.expressions.contains(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(k: &str, v: &str) -> Result<Labels, LabelError> {
        Labels::new([(k.to_owned(), v.to_owned())])
    }

    #[test]
    fn kubernetes_syntax_is_accepted_and_nothing_else() {
        assert!(one("game", "mc").is_ok());
        assert!(one("korpis.dev/tier", "").is_ok());
        assert!(one("a.b-c_d", "x.y-z").is_ok());
        assert_eq!(one("-game", "mc"), Err(LabelError::Key("-game".into())));
        assert_eq!(one("Bad.Prefix/x", "y"), Err(LabelError::Key("Bad.Prefix/x".into())));
        assert_eq!(one("game", "mc!"), Err(LabelError::Value("mc!".into())));
        assert!(one(&"k".repeat(64), "v").is_err());
    }
}
