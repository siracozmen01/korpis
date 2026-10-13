// SPDX-License-Identifier: Apache-2.0

//! What can go wrong.

use connectrpc::{ConnectError, ErrorCode};

/// An SDK failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The control plane, or the identity socket, answered with an error.
    #[error("{0}")]
    Rpc(#[from] ConnectError),
    /// Something local: a malformed address or certificate.
    #[error("{0}")]
    Local(String),
    /// The other side broke the protocol.
    #[error("protocol: {0}")]
    Protocol(String),
}

impl Error {
    /// Whether trying again later may succeed. A refusal (the request is wrong, or the
    /// installation may not do it) will not, and retrying it only hides the cause.
    #[must_use]
    pub fn retryable(&self) -> bool {
        match self {
            Self::Rpc(e) => !matches!(
                e.code,
                ErrorCode::InvalidArgument
                    | ErrorCode::PermissionDenied
                    | ErrorCode::NotFound
                    | ErrorCode::Unauthenticated
                    | ErrorCode::FailedPrecondition
                    | ErrorCode::Unimplemented
            ),
            Self::Local(_) => false,
            Self::Protocol(_) => true,
        }
    }
}
