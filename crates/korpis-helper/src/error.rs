// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the helper answers when it does not do what it was asked.

use korpis_proto::korpis::helper::v1 as helper;

/// Why a verb failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HelperError {
    /// The request is malformed or names what it may not: refused, and logged as such.
    #[error("refused: {0}")]
    Refused(String),
    /// No such volume or class.
    #[error("not found: {0}")]
    NotFound(String),
    /// The kernel or the filesystem refused.
    #[error("{0}")]
    Failed(String),
}

impl From<std::io::Error> for HelperError {
    fn from(e: std::io::Error) -> Self {
        Self::Failed(e.to_string())
    }
}

impl HelperError {
    /// As the protocol carries it.
    #[must_use]
    pub fn to_pb(&self) -> helper::HelperError {
        let code = match self {
            Self::Refused(_) => helper::HelperErrorCode::HELPER_ERROR_CODE_REFUSED,
            Self::NotFound(_) => helper::HelperErrorCode::HELPER_ERROR_CODE_NOT_FOUND,
            Self::Failed(_) => helper::HelperErrorCode::HELPER_ERROR_CODE_FAILED,
        };
        helper::HelperError { code: code.into(), message: self.to_string(), ..Default::default() }
    }
}

/// A result from the helper.
pub type HelperResult<T> = Result<T, HelperError>;

/// That `id` is an identifier the helper may turn into one path component: 1 to 128 bytes of
/// ASCII letters, digits, `_`, `-`, and `.`, starting with a letter or digit, so it can never be
/// `.`, `..`, or contain a separator. Every path the helper touches is built from one of these
/// (§6 of docs/02-architecture.md); nothing else a request carries ever reaches a path.
pub fn identifier(what: &str, id: &str) -> HelperResult<()> {
    let allowed = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.');
    let ok = (1..=128).contains(&id.len())
        && id.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && id.bytes().all(allowed);
    if ok {
        Ok(())
    } else {
        tracing::warn!(%what, id = %id.escape_debug(), "a malformed identifier was refused");
        Err(HelperError::Refused(format!("{what} is not an identifier")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_single_plain_component_is_an_identifier() {
        for good in ["wl_01j8.data", "vol-7", "a"] {
            assert!(identifier("volume", good).is_ok(), "{good}");
        }
        let long = "a".repeat(129);
        for bad in ["", ".", "..", "../etc", "a/b", "/etc", ".hidden", "a b", "a\0b", &long] {
            assert!(identifier("volume", bad).is_err(), "{bad:?}");
        }
    }
}
