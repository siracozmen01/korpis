// SPDX-License-Identifier: Apache-2.0

//! Writing a backup target provider (§5.6 of docs/06-storage.md). The provider holds the
//! target's own credential and never hands it out: for each job it issues access scoped to one
//! workload's prefix (write: put, get, and list, never delete; read: get and list), never for
//! longer than asked, and it lists and deletes for the control plane's prune. No object bytes
//! pass through these calls. A store that cannot scope access may be served by its provider
//! itself: the provider mints the access, verifies the requests made with it
//! ([`crate::sigv4::verify_presigned`]), and answers with the endpoint it serves on
//! (`served_by`) and its certificate in place of an address. A target that can do neither fails
//! `backup.unscoped`. Requires the `provider.backup_target` capability.

pub use crate::provider::Failure;
use crate::provider::{Handler, Outcome, Unsupported};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use std::future::Future;
use std::time::Duration;

/// A backup target.
pub trait BackupTarget: Send + Sync + 'static {
    /// Access to `prefix` for `ttl`, as `mode` allows and nothing more.
    fn grant_access(
        &self,
        prefix: String,
        mode: pb::AccessMode,
        ttl: Duration,
    ) -> impl Future<Output = Result<pb::ObjectAccess, Failure>> + Send;

    /// Keys under `prefix` after `after`, in key order, at most `limit`.
    fn list(
        &self,
        prefix: String,
        after: String,
        limit: u32,
    ) -> impl Future<Output = Result<pb::ObjectList, Failure>> + Send;

    /// Deletes `keys`; how many existed.
    fn delete(&self, keys: Vec<String>) -> impl Future<Output = Result<u32, Failure>> + Send;
}

/// Serves a [`BackupTarget`] as a provider's [`Handler`].
#[derive(Debug, Clone)]
pub struct Backup<P>(pub P);

impl<P: BackupTarget> Handler for Backup<P> {
    async fn call(&self, call: pb::ProviderCall) -> Outcome {
        let answered = match call.request {
            Some(Request::GrantAccess(g)) => {
                let mode = g.mode.as_known().unwrap_or(pb::AccessMode::ACCESS_MODE_UNSPECIFIED);
                let ttl = Duration::from_secs(u64::from(g.ttl_seconds));
                self.0
                    .grant_access(g.prefix.clone(), mode, ttl)
                    .await
                    .map(|a| Outcome::ObjectAccess(Box::new(a)))
            }
            Some(Request::ListObjects(l)) => self
                .0
                .list(l.prefix.clone(), l.after.clone(), l.limit)
                .await
                .map(|l| Outcome::ObjectList(Box::new(l))),
            Some(Request::DeleteObjects(d)) => self.0.delete(d.keys.clone()).await.map(|n| {
                Outcome::ObjectsDeleted(Box::new(pb::ObjectsDeleted {
                    deleted: n,
                    ..Default::default()
                }))
            }),
            _ => return Unsupported.call(call).await,
        };
        answered.unwrap_or_else(Into::into)
    }
}
