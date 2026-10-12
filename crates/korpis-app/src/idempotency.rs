// SPDX-License-Identifier: AGPL-3.0-or-later

//! Idempotent mutations (§6 of docs/10-api.md).
//!
//! The guarantee is the transport's (a retry returns the original response, byte for byte) but
//! its atomicity is the use case's: the response must be recorded in the transaction of the
//! change. So the driving adapter hands the use case an encoder, the use case records what the
//! encoder makes of its result, and a replay hands the recorded bytes back for the adapter to
//! decode. The use case never learns the wire format.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, Recorded, Store};

/// How a mutation's result is recorded for replay.
pub struct Idempotent<'a, T> {
    /// The client's key; required on every mutation.
    pub key: &'a str,
    /// Encodes the result as the response the client receives.
    pub encode: &'a (dyn Fn(&T) -> Vec<u8> + Send + Sync),
}

impl<T> std::fmt::Debug for Idempotent<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Idempotent").field("key", &self.key).finish_non_exhaustive()
    }
}

/// A mutation's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply<T> {
    /// Done now.
    Fresh(T),
    /// Done before, under the same key: the response recorded then.
    Replayed(Vec<u8>),
}

/// The longest idempotency key accepted.
const MAX_KEY: usize = 128;

impl<S: Store, C: Clock> App<S, C> {
    /// The recorded response for this caller's key, if the mutation already happened.
    pub(crate) async fn replay<T>(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, T>,
        operation: &str,
    ) -> Result<Option<Vec<u8>>, AppError> {
        if idem.key.is_empty() || idem.key.len() > MAX_KEY {
            return Err(AppError::invalid("mutation.idempotency_key", "1 to 128 bytes"));
        }
        let Some(recorded) = self.store.recorded(&caller.subject.key, idem.key).await? else {
            return Ok(None);
        };
        if recorded.operation != operation {
            return Err(AppError::conflict(
                "idempotency.key_reused",
                format!("this idempotency key was used for {}", recorded.operation),
            ));
        }
        Ok(Some(recorded.response))
    }
}

/// Records the response and commits: the last step of every mutation.
pub(crate) async fn finish<X: crate::ports::Tx, T>(
    mut tx: X,
    caller: &Caller,
    idem: &Idempotent<'_, T>,
    operation: &str,
    result: T,
) -> Result<Reply<T>, AppError> {
    let recorded = Recorded { operation: operation.to_owned(), response: (idem.encode)(&result) };
    tx.record(&caller.subject.key, idem.key, &recorded).await?;
    tx.commit().await?;
    Ok(Reply::Fresh(result))
}
