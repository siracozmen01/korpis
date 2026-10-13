// SPDX-License-Identifier: AGPL-3.0-or-later

//! containerd, as the driver reaches it: gRPC on its local socket, every request in the
//! `korpis` namespace and nowhere else, and, while a create is under way, under a containerd
//! lease, so the content and snapshots it is assembling are not collected half way.

mod events;
mod import;
mod inspect;
mod lifecycle;
mod unpack;

pub use inspect::Usage;
pub use lifecycle::signal_number;

use crate::error::{DriverError, DriverResult};
use connectrpc::Protocol;
use connectrpc::client::{CallOptions, ClientConfig, Http2Connection, SharedHttp2Connection};
use korpis_containerd::containerd::services as svc;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The namespace every Korpis object lives in. Nothing outside it is read or written.
pub const NAMESPACE: &str = "korpis";
/// The snapshotter the driver unpacks images into.
pub const SNAPSHOTTER: &str = "overlayfs";
/// The runtime containerd runs containers with.
pub const RUNTIME: &str = "io.containerd.runc.v2";
/// How long a lease protects a create that died before it could remove the lease.
const LEASE_EXPIRY: Duration = Duration::from_secs(3600);

type Transport = SharedHttp2Connection;

/// A connection to containerd, in the `korpis` namespace.
#[derive(Clone)]
pub struct Containerd {
    conn: Transport,
    config: ClientConfig,
    /// The node's content store, which the driver reads and never writes.
    content: PathBuf,
    /// Where the driver's cgroups are, under the cgroup v2 root.
    cgroup_parent: String,
    /// What cgroup statistics were last read, to measure CPU as a rate.
    samples: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, inspect::Sample>>>,
}

impl std::fmt::Debug for Containerd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Containerd").field("content", &self.content).finish_non_exhaustive()
    }
}

impl Containerd {
    /// containerd at `socket`, reading content from the store at `content`, and creating the
    /// namespace if it is missing.
    pub async fn connect(socket: &Path, content: &Path) -> DriverResult<Self> {
        let authority: http::Uri =
            "http://localhost".parse().map_err(|e| DriverError::Failed(format!("{e}")))?;
        let conn = Http2Connection::lazy_unix(socket, authority.clone()).shared(256);
        let config = ClientConfig::new(authority)
            .with_protocol(Protocol::Grpc)
            .with_default_header("containerd-namespace", NAMESPACE);
        let this = Self {
            conn,
            config,
            content: content.to_owned(),
            cgroup_parent: format!("/{NAMESPACE}"),
            samples: std::sync::Arc::default(),
        };
        this.namespace().await?;
        Ok(this)
    }

    async fn namespace(&self) -> DriverResult<()> {
        use svc::namespaces::v1 as ns;
        let request = ns::CreateNamespaceRequest {
            namespace: ns::Namespace { name: NAMESPACE.into(), ..Default::default() }.into(),
            ..Default::default()
        };
        match ns::NamespacesClient::new(self.conn.clone(), self.config.clone())
            .create(request)
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.code == connectrpc::ErrorCode::AlreadyExists => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// The cgroup a container runs in.
    fn cgroup(&self, id: &str) -> String {
        format!("{}/{id}", self.cgroup_parent)
    }

    /// The path of the blob `digest` in the content store.
    fn blob(&self, digest: &str) -> DriverResult<PathBuf> {
        let hex =
            korpis_core::content::hex_of(digest).map_err(|e| DriverError::Failed(e.to_string()))?;
        Ok(self.content.join("blobs/sha256").join(hex))
    }

    /// Whether containerd answers.
    pub async fn ping(&self) -> bool {
        let client = svc::version::v1::VersionClient::new(self.conn.clone(), self.config.clone());
        client.version(buffa_types::google::protobuf::Empty::default()).await.is_ok()
    }

    /// Takes a lease that keeps what is made under it until it is released.
    async fn lease(&self) -> DriverResult<String> {
        use svc::leases::v1 as leases;
        let expires = std::time::SystemTime::now() + LEASE_EXPIRY;
        let at = humantime_rfc3339(expires);
        let request = leases::CreateRequest {
            labels: [("containerd.io/gc.expire".to_owned(), at)].into(),
            ..Default::default()
        };
        let created = leases::LeasesClient::new(self.conn.clone(), self.config.clone())
            .create(request)
            .await?
            .into_owned();
        Ok(created.lease.as_option().map(|l| l.id.clone()).unwrap_or_default())
    }

    /// Releases a lease; what it kept is collected once nothing else holds it.
    async fn release(&self, lease: &str) {
        use svc::leases::v1 as leases;
        let request = leases::DeleteRequest { id: lease.to_owned(), ..Default::default() };
        let client = leases::LeasesClient::new(self.conn.clone(), self.config.clone());
        if let Err(e) = client.delete(request).await {
            tracing::warn!(%lease, error = %e, "a containerd lease outlived its create");
        }
    }
}

/// Call options for a request made under `lease`.
fn under(lease: &str) -> CallOptions {
    CallOptions::default().with_header("containerd-lease", lease)
}

/// `t` as RFC 3339 in UTC, to the second.
fn humantime_rfc3339(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil(i64::try_from(days).unwrap_or(0));
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The civil date `days` after 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lease_expiry_is_rfc3339() {
        let t = std::time::UNIX_EPOCH + Duration::from_secs(1_791_849_600 + 3661);
        assert_eq!(humantime_rfc3339(t), "2026-10-13T01:01:01Z");
    }
}
