// SPDX-License-Identifier: AGPL-3.0-or-later

//! Offloaded stream segments (§4 of docs/14-streams.md): a sealed segment, whole, sealed again
//! under the workload's segment key and kept beneath its stream prefix, named by its first offset
//! and its digest, so the same segment offloaded twice is one object and a listing reads in
//! offset order.

use crate::error::{Error, Result};
use crate::repo::Repo;
use crate::seal::Kind;
use crate::store::ObjectStore;

/// The object name of the segment starting at `first_offset` whose digest is `digest`
/// (`sha256:` and 64 hexadecimal digits); `None` for a digest that is not one.
#[must_use]
pub fn segment_id(first_offset: u64, digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    let plain = hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    plain.then(|| format!("{first_offset:020}-{hex}"))
}

/// Offloads `bytes`, the segment starting at `first_offset` with digest `digest`; how many bytes
/// were written.
///
/// # Errors
/// A digest that is not one, or the store refused.
pub async fn put_segment<S: ObjectStore>(
    repo: &Repo<S>,
    (first_offset, digest): (u64, &str),
    bytes: Vec<u8>,
) -> Result<u64> {
    let id = segment_id(first_offset, digest)
        .ok_or_else(|| Error::Refused(format!("{digest:?} is not a segment's digest")))?;
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
        assert_eq!(segment_id(7, &d).unwrap(), format!("00000000000000000007-{}", "ab".repeat(32)));
        assert!(segment_id(7, "sha256:../x").is_none());
        assert!(segment_id(7, &"ab".repeat(32)).is_none());
    }

    #[tokio::test]
    async fn an_offloaded_segment_opens_only_under_its_workload_s_key() {
        let store = MemoryStore::default();
        let d = format!("sha256:{}", "cd".repeat(32));
        let repo = Repo::new(Arc::new(store.clone()), Arc::new(Keys::derive(&[3; KEY_LEN])));
        put_segment(&repo, (0, &d), b"records".to_vec()).await.unwrap();
        let id = segment_id(0, &d).unwrap();
        assert_eq!(repo.segment(&id).await.unwrap(), b"records");
        let other = Repo::new(Arc::new(store), Arc::new(Keys::derive(&[4; KEY_LEN])));
        assert!(other.segment(&id).await.is_err(), "another workload's key opens nothing");
    }
}
