// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the CLI reports when a command fails, and the exit code it reports it with (§2 of
//! docs/13-surface-cli.md). An error the API returned carries its structured `korpis.v1.Error`
//! (§8 of docs/10-api.md), and its code decides the exit code, so a script tells `DENIED` from
//! `CONFLICT` from `UNSATISFIABLE` without reading a word of the message.

use base64::Engine as _;
use buffa::Message as _;
use korpis_proto::korpis::v1 as pb;

/// The exit code of a failure the CLI found itself: a file, an argument, a value.
pub const LOCAL: u8 = 1;
/// The exit code when the control plane could not be reached at all.
pub const UNREACHABLE: u8 = 3;

/// Why a command failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    /// The API refused, and said why.
    #[error("{}: {message}{}", code_name(*code), field_suffix(field))]
    Api {
        /// The error code.
        code: pb::ErrorCode,
        /// The stable reason, e.g. `quota.memory.exceeded`.
        reason: String,
        /// The field, for a validation error.
        field: String,
        /// What it says.
        message: String,
    },
    /// The control plane could not be reached, or answered with no structured error.
    #[error("the control plane: {0}")]
    Transport(String),
    /// Something the CLI found before sending anything.
    #[error("{0}")]
    Local(String),
}

fn field_suffix(field: &str) -> String {
    if field.is_empty() { String::new() } else { format!(" ({field})") }
}

/// The name of a code, as the API documents it.
#[must_use]
pub fn code_name(code: pb::ErrorCode) -> &'static str {
    use pb::ErrorCode as C;
    match code {
        C::ERROR_CODE_INVALID => "INVALID",
        C::ERROR_CODE_UNAUTHENTICATED => "UNAUTHENTICATED",
        C::ERROR_CODE_DENIED => "DENIED",
        C::ERROR_CODE_NOT_FOUND => "NOT_FOUND",
        C::ERROR_CODE_CONFLICT => "CONFLICT",
        C::ERROR_CODE_UNSATISFIABLE => "UNSATISFIABLE",
        C::ERROR_CODE_QUOTA_EXCEEDED => "QUOTA_EXCEEDED",
        C::ERROR_CODE_OVERFLOW => "OVERFLOW",
        C::ERROR_CODE_UNAVAILABLE => "UNAVAILABLE",
        C::ERROR_CODE_INTERNAL | C::ERROR_CODE_UNSPECIFIED => "INTERNAL",
        C::ERROR_CODE_RATE_LIMITED => "RATE_LIMITED",
    }
}

/// The exit code of an API error code: ten and up, one per code, in the order of §8 of
/// docs/10-api.md.
#[must_use]
pub fn exit_code_of(code: pb::ErrorCode) -> u8 {
    use pb::ErrorCode as C;
    match code {
        C::ERROR_CODE_INVALID => 10,
        C::ERROR_CODE_UNAUTHENTICATED => 11,
        C::ERROR_CODE_DENIED => 12,
        C::ERROR_CODE_NOT_FOUND => 13,
        C::ERROR_CODE_CONFLICT => 14,
        C::ERROR_CODE_UNSATISFIABLE => 15,
        C::ERROR_CODE_QUOTA_EXCEEDED => 16,
        C::ERROR_CODE_OVERFLOW => 17,
        C::ERROR_CODE_UNAVAILABLE => 18,
        C::ERROR_CODE_INTERNAL | C::ERROR_CODE_UNSPECIFIED => 19,
        C::ERROR_CODE_RATE_LIMITED => 20,
    }
}

impl CliError {
    /// The process exit code.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Api { code, .. } => exit_code_of(*code),
            Self::Transport(_) => UNREACHABLE,
            Self::Local(_) => LOCAL,
        }
    }

    /// As JSON, for `--json`: the API's own error shape where there is one.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Api { code, reason, field, message } => serde_json::json!({
                "code": code_name(*code), "reason": reason, "field": field, "message": message,
            }),
            other => serde_json::json!({ "code": "CLIENT", "message": other.to_string() }),
        }
    }
}

/// The structured error a Connect error carries, if it carries one.
fn structured(e: &connectrpc::ConnectError) -> Option<pb::Error> {
    let detail = e.details.iter().find(|d| d.type_url.ends_with("korpis.v1.Error"))?;
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(detail.value.as_deref()?.trim_end_matches('='))
        .ok()?;
    pb::Error::decode_from_slice(&bytes).ok()
}

impl From<connectrpc::ConnectError> for CliError {
    fn from(e: connectrpc::ConnectError) -> Self {
        match structured(&e) {
            Some(s) => Self::Api {
                code: s.code.as_known().unwrap_or(pb::ErrorCode::ERROR_CODE_INTERNAL),
                reason: s.reason,
                field: s.field,
                message: s.message,
            },
            None => Self::Transport(e.message.unwrap_or_else(|| format!("{:?}", e.code))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_its_own_exit_code() {
        use pb::ErrorCode as C;
        let codes = [
            C::ERROR_CODE_INVALID,
            C::ERROR_CODE_UNAUTHENTICATED,
            C::ERROR_CODE_DENIED,
            C::ERROR_CODE_NOT_FOUND,
            C::ERROR_CODE_CONFLICT,
            C::ERROR_CODE_UNSATISFIABLE,
            C::ERROR_CODE_QUOTA_EXCEEDED,
            C::ERROR_CODE_OVERFLOW,
            C::ERROR_CODE_UNAVAILABLE,
            C::ERROR_CODE_INTERNAL,
        ];
        let exits: std::collections::BTreeSet<u8> =
            codes.iter().map(|c| exit_code_of(*c)).collect();
        assert_eq!(exits.len(), codes.len());
        assert!(!exits.contains(&LOCAL) && !exits.contains(&UNREACHABLE) && !exits.contains(&2));
    }

    #[test]
    fn a_structured_detail_is_read_back() {
        let detail = pb::Error {
            code: pb::ErrorCode::ERROR_CODE_CONFLICT.into(),
            reason: "version.mismatch".into(),
            message: "the workload moved".into(),
            ..Default::default()
        };
        let e = connectrpc::ConnectError::new(connectrpc::ErrorCode::Aborted, "x")
            .with_detail(connectrpc::ErrorDetail::from_message("korpis.v1.Error", &detail));
        let cli = CliError::from(e);
        assert_eq!(cli.exit_code(), 14);
        assert!(cli.to_string().starts_with("CONFLICT: the workload moved"), "{cli}");
    }
}
