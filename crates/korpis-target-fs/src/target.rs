// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider: a grant minted for the prefix and mode asked, lasting as asked, answered with the
//! endpoint the store is served on and its certificate in place of an address; and the listing
//! and deletion a prune asks for, on the volume itself. Keys are the control plane's, whole.

use crate::store::Store;
use crate::tokens::{Mode, Tokens};
use korpis_extension::backup::{BackupTarget, Failure};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::object_access::Protocol;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// The one bucket the store serves.
pub const BUCKET: &str = "korpis";
/// The region its requests are signed for.
pub const REGION: &str = "local";
/// The longest grant, the control plane's own bound.
const LONGEST: Duration = Duration::from_secs(12 * 3600);

/// The provider.
#[derive(Debug, Clone)]
pub struct FsTarget {
    store: Store,
    tokens: Arc<Tokens>,
    endpoint: String,
    certificate: Vec<u8>,
}

fn refused(reason: &str, message: impl std::fmt::Display) -> Failure {
    Failure { reason: reason.into(), message: message.to_string(), retryable: false }
}

fn failed(message: impl std::fmt::Display) -> Failure {
    Failure { reason: "store.failed".into(), message: message.to_string(), retryable: true }
}

fn timestamp(t: SystemTime) -> buffa_types::google::protobuf::Timestamp {
    let since = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    buffa_types::google::protobuf::Timestamp {
        seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(since.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    }
}

/// Whether `prefix` is a plain prefix: relative, ending in `/`, each component letters, digits,
/// `.`, `_`, and `-`, none `.` or `..`.
#[must_use]
pub fn is_plain_prefix(prefix: &str) -> bool {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    prefix.len() <= 1024
        && prefix.strip_suffix('/').is_some_and(|body| {
            body.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && c.chars().all(plain))
        })
}

impl FsTarget {
    /// The provider over `store`, minting into `tokens`, served on the endpoint `endpoint` with
    /// `certificate`.
    #[must_use]
    pub fn new(store: Store, tokens: Arc<Tokens>, endpoint: String, certificate: Vec<u8>) -> Self {
        Self { store, tokens, endpoint, certificate }
    }

    /// A grant of `mode` to `prefix` for `ttl`, as the access a job is answered with.
    fn grant(
        &self,
        prefix: String,
        mode: pb::AccessMode,
        ttl: Duration,
    ) -> Result<pb::ObjectAccess, Failure> {
        if !is_plain_prefix(&prefix) {
            return Err(refused("access.prefix", format!("{prefix:?} is not a plain prefix")));
        }
        if ttl.is_zero() || ttl > LONGEST {
            return Err(refused("access.ttl", "up to 43200 seconds"));
        }
        let mode = match mode {
            pb::AccessMode::ACCESS_MODE_WRITE => Mode::Write,
            pb::AccessMode::ACCESS_MODE_READ => Mode::Read,
            pb::AccessMode::ACCESS_MODE_UNSPECIFIED => {
                return Err(refused("access.mode", "write or read"));
            }
        };
        let g = self.tokens.mint(&prefix, mode, (SystemTime::now(), ttl)).map_err(failed)?;
        let s3 = pb::S3Access {
            region: REGION.into(),
            bucket: BUCKET.into(),
            key_prefix: prefix,
            access_key_id: g.access_key_id,
            secret_access_key: g.secret,
            session_token: g.session_token,
            path_style: true,
            served_by: self.endpoint.clone(),
            certificate: self.certificate.clone(),
            ..Default::default()
        };
        Ok(pb::ObjectAccess {
            expires_at: timestamp(g.expires).into(),
            protocol: Some(Protocol::S3(Box::new(s3))),
            ..Default::default()
        })
    }
}

impl BackupTarget for FsTarget {
    fn grant_access(
        &self,
        prefix: String,
        mode: pb::AccessMode,
        ttl: Duration,
    ) -> impl Future<Output = Result<pb::ObjectAccess, Failure>> + Send {
        std::future::ready(self.grant(prefix, mode, ttl))
    }

    async fn list(
        &self,
        prefix: String,
        after: String,
        limit: u32,
    ) -> Result<pb::ObjectList, Failure> {
        if !is_plain_prefix(&prefix) {
            return Err(refused("list.prefix", format!("{prefix:?} is not a plain prefix")));
        }
        let limit = usize::try_from(limit.clamp(1, 1000)).unwrap_or(1000);
        let store = self.store.clone();
        let page = tokio::task::spawn_blocking(move || store.list(&prefix, &after, limit))
            .await
            .map_err(failed)?
            .map_err(failed)?;
        let objects = page
            .objects
            .into_iter()
            .map(|o| pb::StoredObject {
                key: o.key,
                size: o.size,
                modified: timestamp(o.modified).into(),
                ..Default::default()
            })
            .collect();
        Ok(pb::ObjectList { objects, more: page.more, ..Default::default() })
    }

    async fn delete(&self, keys: Vec<String>) -> Result<u32, Failure> {
        if keys.len() > 1000 {
            return Err(refused("delete.keys", "at most 1000 keys"));
        }
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.delete(&keys)).await.map_err(failed)?.map_err(
            |e| match e.kind() {
                std::io::ErrorKind::InvalidInput => refused("delete.keys", e),
                _ => failed(e),
            },
        )
    }
}
