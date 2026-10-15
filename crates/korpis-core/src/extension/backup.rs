// SPDX-License-Identifier: AGPL-3.0-or-later

//! A backup target provider's calls (§5.6 of docs/06-storage.md): access scoped to one
//! workload's prefix for one job, and the listing and deletion the control plane prunes with.
//! Object bytes never travel through these calls.

use std::time::{Duration, SystemTime};

/// The shortest access a job is granted: the shortest a store's token service issues, so that no
/// target grants longer than asked, which a prune's sweep relies on (§5.5 of
/// docs/06-storage.md).
pub const MIN_ACCESS: Duration = Duration::from_secs(15 * 60);

/// The longest access a job is granted: a job longer than this asks again.
pub const MAX_ACCESS: Duration = Duration::from_secs(12 * 3600);

/// The most keys one listing or deletion handles.
pub const MAX_KEYS: usize = 1000;

/// What access is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// Put, get, and list under the prefix; never delete. A backup reads its parent's record
    /// and trees to take unchanged files from it (Finding 41 of docs/23-walkthroughs.md).
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
    /// Whether the bucket is addressed in the path rather than the host name.
    pub path_style: bool,
    /// For a store its provider serves itself, the provider's endpoint it serves on, in place of
    /// an address, until the control plane puts the endpoint's address in `endpoint` (§5.6 of
    /// docs/06-storage.md).
    pub served_by: String,
    /// The one certificate, DER, such a store presents; empty for system roots.
    pub certificate: Vec<u8>,
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

/// Whether `prefix` is a repository prefix: relative, `/`-terminated, at most 1024 bytes, each
/// component letters, digits, `.`, `_`, and `-`, and none `.` or `..`. A target scopes access by
/// writing the prefix into a policy, where `*`, `?`, and `${…}` are patterns and variables, so a
/// prefix holds nothing a policy could read as more than itself (Finding 42 of
/// docs/23-walkthroughs.md).
#[must_use]
pub fn is_prefix(prefix: &str) -> bool {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    prefix.len() <= 1024
        && prefix.strip_suffix('/').is_some_and(|body| {
            body.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && c.chars().all(plain))
        })
}

/// The largest certificate an access carries.
pub const MAX_CERTIFICATE: usize = 16 << 10;

/// Whether `a` says where its store is in exactly one way: an address, or, for a store its
/// provider serves itself, the name of the provider's endpoint, which an endpoint name is
/// (lower-case letters, digits, and `-`, at most 63); and carries a certificate only it may.
#[must_use]
pub fn is_located(a: &S3Access) -> bool {
    let name = |n: &str| {
        (1..=63).contains(&n.len())
            && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let located = if a.served_by.is_empty() {
        !a.endpoint.is_empty()
    } else {
        a.endpoint.is_empty() && name(&a.served_by)
    };
    located && a.certificate.len() <= MAX_CERTIFICATE
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
        assert!(is_prefix("korpis.backups/nightly-1/workloads/0192_a/"));
        for bad in [
            "workloads/wl_1",
            "/abs/",
            "a/../b/",
            "a//b/",
            "/",
            "",
            "a*/",
            "a?/",
            "${aws:username}/",
            "a b/",
            "ä/",
        ] {
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
                path_style: false,
                served_by: String::new(),
                certificate: Vec::new(),
            },
        };
        let r = a.redacted();
        assert_eq!((r.s3.bucket.as_str(), r.s3.access_key_id.as_str()), ("b", "AK"));
        assert!(r.s3.secret_access_key.is_empty() && r.s3.session_token.is_empty());
        assert!(!format!("{a:?}").contains("secret"), "never printed");
    }
}
