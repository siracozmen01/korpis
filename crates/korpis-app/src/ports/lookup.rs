// SPDX-License-Identifier: AGPL-3.0-or-later

//! Looking up DNS records, as the control plane verifies a domain (§3.2 of
//! docs/07-networking.md): through public DNS, never through anything a tenant controls.

use std::future::Future;
use std::pin::Pin;

/// Why a lookup gave no answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LookupError {
    /// The name has no record of the kind asked for.
    #[error("no record")]
    NotFound,
    /// The resolver could not be asked, or did not answer.
    #[error("the lookup failed: {0}")]
    Failed(String),
}

/// Looks up TXT records.
pub trait TxtLookup: Send + Sync + std::fmt::Debug {
    /// Every TXT string at `name`, each record's strings joined.
    fn txt<'a>(
        &'a self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>>;
}
