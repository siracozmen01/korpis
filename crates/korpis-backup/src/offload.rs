// SPDX-License-Identifier: AGPL-3.0-or-later

//! Offloaded stream segments (§4 of docs/14-streams.md): a sealed segment, whole, sealed again
//! under the workload's segment key and kept beneath its stream prefix, named by its first offset
//! and its digest, so the same segment offloaded twice is one object and a listing reads in
//! offset order.

use crate::error::{Error, Result};
use crate::repo::Repo;
use crate::seal::Kind;
use crate::store::ObjectStore;

/// Whether `s` is `n` lowercase hexadecimal digits.
fn hex_of(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The object name of the segment starting at `first_offset` whose digest is `digest`
/// (`sha256:` and 64 hexadecimal digits), beneath `runs/` and its run for a run's stream (`run_`
/// and 32 hexadecimal digits), whose offsets start from zero again; `None` for a digest or a run
/// that is not one. The run is in the name, which sealing binds, so an object is opened only as
/// the run it was offloaded for.
#[must_use]
pub fn segment_id(run: Option<&str>, first_offset: u64, digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:").filter(|h| hex_of(h, 64))?;
    let runs = match run {
        None => String::new(),
        Some(r) => {
            format!("runs/{}/", r.strip_prefix("run_").filter(|h| hex_of(h, 32)).map(|_| r)?)
        }
    };
    Some(format!("{runs}{first_offset:020}-{hex}"))
}

/// Offloads `bytes`, the segment starting at `first_offset` with digest `digest`; how many bytes
/// were written.
///
/// # Errors
/// A digest that is not one, or the store refused.
pub async fn put_segment<S: ObjectStore>(
    repo: &Repo<S>,
    (run, first_offset, digest): (Option<&str>, u64, &str),
    bytes: Vec<u8>,
) -> Result<u64> {
    let id = segment_id(run, first_offset, digest).ok_or_else(|| {
        Error::Refused(format!("{digest:?} of {run:?} is not a segment's digest and run"))
    })?;
    repo.put(Kind::Segment, &id, bytes).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{KEY_LEN, Keys};
    use crate::store::MemoryStore;
    use std::sync::Arc;

    #[test]
    fn a_segment_is_named_by_its_offset_and_digest_and_nothing_else_is_one() {
        let d = format!("sha256:{}", "ab".repeat(32));
        let named = segment_id(None, 7, &d).unwrap();
        assert_eq!(named, format!("00000000000000000007-{}", "ab".repeat(32)));
        assert!(segment_id(None, 7, "sha256:../x").is_none());
        assert!(segment_id(None, 7, &"ab".repeat(32)).is_none());
        let run = format!("run_{}", "cd".repeat(16));
        let named = segment_id(Some(&run), 7, &d).unwrap();
        assert_eq!(named, format!("runs/{run}/00000000000000000007-{}", "ab".repeat(32)));
        assert!(segment_id(Some("run_../.."), 7, &d).is_none(), "a run that is not one");
    }

    #[tokio::test]
    async fn an_offloaded_segment_opens_only_under_its_workload_s_key() {
        let store = MemoryStore::default();
        let d = format!("sha256:{}", "cd".repeat(32));
        let repo = Repo::new(Arc::new(store.clone()), Arc::new(Keys::derive(&[3; KEY_LEN])));
        put_segment(&repo, (None, 0, &d), b"records".to_vec()).await.unwrap();
        let id = segment_id(None, 0, &d).unwrap();
        assert_eq!(repo.segment(&id).await.unwrap(), b"records");
        let other = Repo::new(Arc::new(store), Arc::new(Keys::derive(&[4; KEY_LEN])));
        assert!(other.segment(&id).await.is_err(), "another workload's key opens nothing");
    }
}
