// SPDX-License-Identifier: AGPL-3.0-or-later

//! Asking a change again while the store answers that another contended with it.

use std::time::Duration;

/// What `call` answers, asked again, a little later, while it is refused `ABORTED`: a change undone
/// because another contended with it, which its idempotency key lets the same request retry.
pub async fn contended<T, F>(mut call: impl FnMut() -> F) -> T
where
    F: Future<Output = Result<T, connectrpc::ConnectError>>,
{
    for _ in 0..20 {
        match call().await {
            Err(e) if e.code == connectrpc::ErrorCode::Aborted => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            answered => return answered.unwrap(),
        }
    }
    call().await.unwrap()
}
