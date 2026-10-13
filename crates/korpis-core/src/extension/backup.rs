// SPDX-License-Identifier: AGPL-3.0-or-later

//! A backup target provider's calls (§5.6 of docs/06-storage.md): access scoped to one
//! workload's prefix for one job, and the listing and deletion the control plane prunes with.
//! Object bytes never travel through these calls.

use std::time::{Duration, SystemTime};

/// The shortest access a job is granted.
pub const MIN_ACCESS: Duration = Duration::from_secs(60);

/// The longest access a job is granted: a job longer than this asks again.
pub const MAX_ACCESS: Duration = Duration::from_secs(12 * 3600);

/// The most keys one listing or deletion handles.
pub const MAX_KEYS: usize = 1000;

/// What access is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// Put new objects and list them under the prefix; never delete.
    Write,
    /// Get and list under the prefix.
    Read,
}

/// S3-compatible access to a prefix.
#[derive(Clone, PartialEq, Eq)]
pub struct S3Access {
    /// The service's address.
    pub endpoint: String,
    /// Its region.
    pub region: String,
    /// The bucket.
    pub bucket: String,
    /// Where the granted prefix lives in the bucket.
    pub key_prefix: String,
    /// The access key.
    pub access_key_id: String,
    /// The secret; empty once redacted from the record of the call.
    pub secret_access_key: String,
    /// The session token, for temporary credentials.
    pub session_token: String,
}

impl std::fmt::Debug for S3Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Access")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("key_prefix", &self.key_prefix)
            .finish_non_exhaustive()
    }
}

/// Access a node uses directly, until it expires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectAccess {
    /// When it stops working.
    pub expires_at: SystemTime,
    /// How to use it.
    pub s3: S3Access,
}

impl ObjectAccess {
    /// The same access with its secrets removed, as the record of the call keeps it once read.
    #[must_use]
    pub fn redacted(&self) -> Self {
        let mut r = self.clone();
        r.s3.secret_access_key.clear();
        r.s3.session_token.clear();
        r
    }
}

/// One object in a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    /// Its key.
    pub key: String,
    /// Its size in bytes.
    pub size: u64,
    /// When it was last written.
    pub modified: SystemTime,
}

/// Whether `prefix` is a repository prefix: relative, `/`-terminated, no empty, `.`, or `..`
/// components, at most 1024 bytes.
#[must_use]
pub fn is_prefix(prefix: &str) -> bool {
    prefix.len() <= 1024
        && prefix.strip_suffix('/').is_some_and(|body| {
            !body.is_empty()
                && !body.starts_with('/')
                && !body.chars().any(char::is_control)
                && body.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
        })
}

/// Whether `key` lies beneath `prefix`, with something after it.
#[must_use]
pub fn is_beneath(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && !rest.split('/').any(|c| c == ".."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_is_a_relative_directory_and_a_key_must_lie_beneath_it() {
        assert!(is_prefix("workloads/wl_1/"));
        for bad in ["workloads/wl_1", "/abs/", "a/../b/", "a//b/", "/", ""] {
            assert!(!is_prefix(bad), "{bad:?}");
        }
        assert!(is_beneath("workloads/wl_1/chunks/ab", "workloads/wl_1/"));
        assert!(!is_beneath("workloads/wl_2/chunks/ab", "workloads/wl_1/"));
        assert!(!is_beneath("workloads/wl_1/", "workloads/wl_1/"));
        assert!(!is_beneath("workloads/wl_1/../wl_2/x", "workloads/wl_1/"));
    }

    #[test]
    fn a_redacted_access_keeps_where_and_drops_the_secrets() {
        let a = ObjectAccess {
            expires_at: SystemTime::UNIX_EPOCH,
            s3: S3Access {
                endpoint: "https://s3.example".into(),
                region: "eu".into(),
                bucket: "b".into(),
                key_prefix: "r/workloads/wl_1/".into(),
                access_key_id: "AK".into(),
                secret_access_key: "secret".into(),
                session_token: "token".into(),
            },
        };
        let r = a.redacted();
        assert_eq!((r.s3.bucket.as_str(), r.s3.access_key_id.as_str()), ("b", "AK"));
        assert!(r.s3.secret_access_key.is_empty() && r.s3.session_token.is_empty());
        assert!(!format!("{a:?}").contains("secret"), "never printed");
    }
}
