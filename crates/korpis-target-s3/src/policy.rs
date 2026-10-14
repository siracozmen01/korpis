// SPDX-License-Identifier: AGPL-3.0-or-later

//! The session policy a job's temporary credential carries (§5.5 and §5.6 of
//! docs/06-storage.md): objects beneath one key prefix of one bucket, read and written for a
//! backup, read for a restore, never deleted; and listing only beneath that prefix. The store
//! grants what both this and the provider's own credential allow, so the credential's policy
//! must allow at least this. The prefix is plain characters only (Finding 42 of
//! docs/23-walkthroughs.md), so nothing in it is read as a pattern.

use serde_json::{Value, json};

/// What the credential is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A backup: put, get, and list; a backup reads its parent's record and trees.
    Write,
    /// A restore or a read: get and list.
    Read,
}

/// The policy for `mode` on `key_prefix` of `bucket`.
#[must_use]
pub fn session_policy(bucket: &str, key_prefix: &str, mode: Mode) -> Value {
    let objects = match mode {
        Mode::Write => json!(["s3:GetObject", "s3:PutObject"]),
        Mode::Read => json!(["s3:GetObject"]),
    };
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": objects,
                "Resource": [format!("arn:aws:s3:::{bucket}/{key_prefix}*")],
            },
            {
                "Effect": "Allow",
                "Action": ["s3:ListBucket"],
                "Resource": [format!("arn:aws:s3:::{bucket}")],
                "Condition": { "StringLike": { "s3:prefix": [format!("{key_prefix}*")] } },
            },
        ],
    })
}

/// Whether `prefix` is one the provider scopes: relative, `/`-terminated, each component plain
/// characters and neither `.` nor `..`. The control plane asks only for such prefixes; the
/// provider does not take that on trust.
#[must_use]
pub fn is_plain_prefix(prefix: &str) -> bool {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    prefix.len() <= 1024
        && prefix.strip_suffix('/').is_some_and(|body| {
            body.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && c.chars().all(plain))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_may_read_and_write_its_prefix_and_a_restore_only_read() {
        let w = session_policy("backups", "korpis/workloads/w1/", Mode::Write);
        assert_eq!(w["Statement"][0]["Action"], json!(["s3:GetObject", "s3:PutObject"]));
        assert_eq!(
            w["Statement"][0]["Resource"],
            json!(["arn:aws:s3:::backups/korpis/workloads/w1/*"])
        );
        assert_eq!(
            w["Statement"][1]["Condition"]["StringLike"]["s3:prefix"],
            json!(["korpis/workloads/w1/*"])
        );
        let r = session_policy("backups", "korpis/workloads/w1/", Mode::Read);
        assert_eq!(r["Statement"][0]["Action"], json!(["s3:GetObject"]));
        let text = w.to_string();
        assert!(!text.contains("Delete"), "nothing is ever deleted with it");
    }

    #[test]
    fn only_a_plain_prefix_is_scoped() {
        assert!(is_plain_prefix("korpis/workloads/0192-ab/"));
        for bad in ["a*/", "${aws:username}/", "a/../", "/a/", "a", "a//b/", "a?/"] {
            assert!(!is_plain_prefix(bad), "{bad}");
        }
    }
}
