// SPDX-License-Identifier: AGPL-3.0-or-later

//! Idempotency keys (§6 of docs/10-api.md): a repeated key returns the original outcome.

use super::StoreResult;
use std::future::Future;

/// A response recorded for a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The operation it answered; a key reused for another operation is refused.
    pub operation: String,
    /// The response as the driving adapter encoded it.
    pub response: Vec<u8>,
}

/// Looking up keys.
pub trait IdempotencyRead {
    /// The response recorded for `(subject_key, key)`, if any.
    fn recorded(
        &self,
        subject_key: &str,
        key: &str,
    ) -> impl Future<Output = StoreResult<Option<Recorded>>> + Send;
}

/// Recording keys, in the transaction of the change they answer, so a key is never recorded for
/// a change that did not commit nor a change committed without its key.
pub trait IdempotencyWrite {
    /// Records a response. A concurrent request with the same key is
    /// [`Unique::IdempotencyKey`](super::Unique::IdempotencyKey).
    fn record(
        &mut self,
        subject_key: &str,
        key: &str,
        recorded: &Recorded,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
