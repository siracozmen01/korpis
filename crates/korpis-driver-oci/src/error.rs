// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the driver answers when it does not do what it was asked.

use crate::validate::Violation;

/// Why a driver call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DriverError {
    /// The spec breaks the tier's invariants: a security event, refused and reported.
    #[error("refused: {0}")]
    Refused(#[from] Violation),
    /// The workload cannot run here, for a reason the operator can act on.
    #[error("{0}")]
    Rejected(String),
    /// No such runtime object.
    #[error("no runtime object {0}")]
    NotFound(String),
    /// containerd could not be reached.
    #[error("containerd is unavailable: {0}")]
    Unavailable(String),
    /// containerd or the kernel refused.
    #[error("{0}")]
    Failed(String),
}

impl From<connectrpc::ConnectError> for DriverError {
    fn from(e: connectrpc::ConnectError) -> Self {
        use connectrpc::ErrorCode as C;
        let message = e.message.clone().unwrap_or_else(|| format!("{:?}", e.code));
        match e.code {
            C::NotFound => Self::NotFound(message),
            C::Unavailable | C::DeadlineExceeded => Self::Unavailable(message),
            _ => Self::Failed(message),
        }
    }
}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        Self::Failed(e.to_string())
    }
}

/// A result from the driver.
pub type DriverResult<T> = Result<T, DriverError>;
