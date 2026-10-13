// SPDX-License-Identifier: AGPL-3.0-or-later

//! Errors on the wire (§8 of docs/10-api.md): a Connect code for transports, and a structured
//! `korpis.v1.Error` detail that clients branch on. Never a message string to parse.

use connectrpc::{ConnectError, ErrorCode, ErrorDetail};
use korpis_app::AppError;
use korpis_proto::korpis::v1 as pb;

/// The `korpis.v1.Error` detail's type name.
pub const DETAIL_TYPE: &str = "korpis.v1.Error";

struct Wire<'a> {
    code: pb::ErrorCode,
    reason: &'a str,
    field: &'a str,
    message: &'a str,
    retryable: bool,
}

fn transport(code: pb::ErrorCode) -> ErrorCode {
    match code {
        pb::ErrorCode::ERROR_CODE_INVALID => ErrorCode::InvalidArgument,
        pb::ErrorCode::ERROR_CODE_UNAUTHENTICATED => ErrorCode::Unauthenticated,
        pb::ErrorCode::ERROR_CODE_DENIED => ErrorCode::PermissionDenied,
        pb::ErrorCode::ERROR_CODE_NOT_FOUND => ErrorCode::NotFound,
        pb::ErrorCode::ERROR_CODE_CONFLICT => ErrorCode::Aborted,
        pb::ErrorCode::ERROR_CODE_UNSATISFIABLE => ErrorCode::FailedPrecondition,
        pb::ErrorCode::ERROR_CODE_QUOTA_EXCEEDED | pb::ErrorCode::ERROR_CODE_OVERFLOW => {
            ErrorCode::ResourceExhausted
        }
        pb::ErrorCode::ERROR_CODE_UNAVAILABLE => ErrorCode::Unavailable,
        pb::ErrorCode::ERROR_CODE_INTERNAL | pb::ErrorCode::ERROR_CODE_UNSPECIFIED => {
            ErrorCode::Internal
        }
    }
}

fn build(w: &Wire<'_>) -> ConnectError {
    let detail = pb::Error {
        code: w.code.into(),
        reason: w.reason.to_owned(),
        field: w.field.to_owned(),
        retryable: w.retryable,
        message: w.message.to_owned(),
        ..Default::default()
    };
    ConnectError::new(transport(w.code), w.message.to_owned())
        .with_detail(ErrorDetail::from_message(DETAIL_TYPE, &detail))
}

/// A malformed request field, found while translating the request.
pub fn invalid(field: &str, message: impl AsRef<str>) -> ConnectError {
    build(&Wire {
        code: pb::ErrorCode::ERROR_CODE_INVALID,
        reason: "request.invalid",
        field,
        message: message.as_ref(),
        retryable: false,
    })
}

/// A failure of our own: logged with its cause, reported without it.
pub fn internal(cause: &dyn std::fmt::Display) -> ConnectError {
    tracing::error!(%cause, "internal error");
    build(&Wire {
        code: pb::ErrorCode::ERROR_CODE_INTERNAL,
        reason: "internal",
        field: "",
        message: "internal error",
        retryable: true,
    })
}

/// A data-path session refused, by the relay or by the node (§3.1 of
/// docs/02-architecture.md). Only an unavailable node is worth trying again.
#[must_use]
pub fn session_refused(code: pb::ErrorCode, reason: &str, message: &str) -> ConnectError {
    let retryable = code == pb::ErrorCode::ERROR_CODE_UNAVAILABLE;
    build(&Wire { code, reason, field: "", message, retryable })
}

/// A refusal that is about who is asking or what they may use, as the client sees it.
fn refusal(e: AppError) -> ConnectError {
    use pb::ErrorCode as C;
    match e {
        AppError::Unauthenticated => build(&Wire {
            code: C::ERROR_CODE_UNAUTHENTICATED,
            reason: "credential.missing_or_invalid",
            field: "",
            message: "a valid bearer credential is required",
            retryable: false,
        }),
        // DENIED names the missing action and never the grant that would have granted it.
        AppError::Denied { action } => build(&Wire {
            code: C::ERROR_CODE_DENIED,
            reason: "authorization.denied",
            field: "",
            message: &format!("missing action {action}"),
            retryable: false,
        }),
        AppError::FieldDenied { field, action } => build(&Wire {
            code: C::ERROR_CODE_DENIED,
            reason: "authorization.field_denied",
            field: &field,
            message: &format!("setting {field} needs {action} over the parent organization"),
            retryable: false,
        }),
        AppError::Untrusted { message } => build(&Wire {
            code: C::ERROR_CODE_DENIED,
            reason: "recipe.untrusted",
            field: "body.recipe",
            message: &message,
            retryable: false,
        }),
        other => internal(&format!("not a refusal: {other}")),
    }
}

/// A use case's answer as the client sees it.
#[must_use]
pub fn from_app(e: AppError) -> ConnectError {
    use pb::ErrorCode as C;
    match e {
        AppError::Invalid { field, message } => invalid(&field, message),
        e @ (AppError::Unauthenticated
        | AppError::Denied { .. }
        | AppError::FieldDenied { .. }
        | AppError::Untrusted { .. }) => refusal(e),
        // Also what a caller who may not read the object is told, so existence is gated by
        // read authority.
        AppError::NotFound { kind } => build(&Wire {
            code: C::ERROR_CODE_NOT_FOUND,
            reason: "object.not_found",
            field: "",
            message: &format!("{kind} not found"),
            retryable: false,
        }),
        AppError::Conflict { reason, message } => build(&Wire {
            code: C::ERROR_CODE_CONFLICT,
            reason,
            field: "",
            message: &message,
            retryable: reason == "idempotency.in_flight",
        }),
        AppError::Unsatisfiable { reason, message } => build(&Wire {
            code: C::ERROR_CODE_UNSATISFIABLE,
            reason,
            field: "",
            message: &message,
            retryable: false,
        }),
        AppError::QuotaExceeded { dimension, message } => build(&Wire {
            code: C::ERROR_CODE_QUOTA_EXCEEDED,
            reason: "quota.exceeded",
            field: dimension,
            message: &message,
            retryable: false,
        }),
        AppError::NotYet { what, phase } => ConnectError::unimplemented(format!(
            "{what} arrive in Phase {phase} (§4 of docs/20-roadmap.md)"
        )),
        AppError::Unavailable { what, detail } => {
            tracing::warn!(%detail, "{what} is unavailable");
            build(&Wire {
                code: C::ERROR_CODE_UNAVAILABLE,
                reason: "dependency.unavailable",
                field: "",
                message: &format!("{what} is unavailable; retry"),
                retryable: true,
            })
        }
        AppError::Internal(cause) => internal(&cause),
    }
}
