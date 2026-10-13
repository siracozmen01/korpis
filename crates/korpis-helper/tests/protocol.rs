// SPDX-License-Identifier: AGPL-3.0-or-later

//! The helper's protocol end to end: the helper serving on its socket from its one thread, the
//! supervisor's client calling it. Volumes are made and removed, ranges recorded, a host range
//! and a path refused, an unknown verb answered as one, and a peer the kernel does not name
//! never heard. Volume ownership needs root, so that test runs where it is root.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use buffa::Message as _;
use korpis_agent::host::privileged::HelperClient;
use korpis_core::workload::assignment::IdRange;
use korpis_helper::helper::Helper;
use korpis_helper::ids::Ledger;
use korpis_helper::serve::{bound, serve};
use korpis_helper::storage::Class;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::Duration;

fn own() -> u32 {
    std::fs::metadata("/proc/self").unwrap().uid()
}

/// A helper serving in `dir` to `allowed` until it has been idle a while; its client.
fn started(dir: &Path, allowed: BTreeSet<u32>) -> HelperClient {
    let class = Class::open("local", &dir.join("volumes")).unwrap();
    let mut h = Helper::new(vec![class], Ledger::open(&dir.join("state")).unwrap());
    let socket = dir.join("helper.sock");
    let listener = bound(&socket).unwrap();
    std::thread::spawn(move || serve(&listener, &allowed, &mut h, Some(Duration::from_secs(5))));
    HelperClient::new(&socket)
}

#[tokio::test]
async fn volumes_are_made_owned_and_removed_and_nothing_else_is_reachable() {
    if own() != 0 {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = started(dir.path(), [0].into());
    let classes = c.describe_storage().await.unwrap();
    assert_eq!(classes.len(), 1);
    assert_eq!(classes[0].backend.as_known(), Some(pb::StorageBackend::STORAGE_BACKEND_DIRECTORY));
    assert!(!classes[0].capabilities.as_option().unwrap().quota_enforced, "said plainly");

    let r = helper::EnsureVolumeRequest {
        volume: "wl_1.data".into(),
        workload: "wl_1".into(),
        size_bytes: 1 << 30,
        ..Default::default()
    };
    let made = c.ensure_volume(&r).await.unwrap();
    assert!(made.created && !made.quota_enforced);
    let m = std::fs::metadata(&made.host_path).unwrap();
    assert_eq!((m.uid(), m.gid(), m.permissions().mode() & 0o777), (0, 0, 0o755));
    assert!(!c.ensure_volume(&r).await.unwrap().created, "idempotent");
    assert!(!c.volume_usage("wl_1.data").await.unwrap().accounted, "read, never computed");

    let escape = helper::EnsureVolumeRequest { volume: "../etc".into(), ..r.clone() };
    assert!(c.ensure_volume(&escape).await.unwrap_err().0.contains("REFUSED"));
    let range = IdRange::of_slot(3).unwrap();
    c.ensure_id_range("wl_1", range.base, range.count).await.unwrap();
    assert!(c.ensure_id_range("wl_1", 0, 65536).await.is_err(), "root's ids are no workload's");

    assert!(c.delete_volume("wl_1.data").await.unwrap());
    assert!(!Path::new(&made.host_path).exists());
    assert!(!c.delete_volume("wl_1.data").await.unwrap(), "already gone");
}

#[tokio::test]
async fn an_unknown_verb_and_an_unnamed_peer_get_nothing_done() {
    let dir = tempfile::tempdir().unwrap();
    let _c = started(dir.path(), [own()].into());
    // An envelope with no verb this helper knows, as a newer supervisor's would read here.
    let mut s = std::os::unix::net::UnixStream::connect(dir.path().join("helper.sock")).unwrap();
    korpis_helper::frame::write(&mut s, &[]).unwrap();
    let body = korpis_helper::frame::read(&mut s).unwrap().unwrap();
    let answer = helper::HelperResponse::decode_from_slice(&body).unwrap();
    assert!(matches!(
        answer.result,
        Some(helper::helper_response::Result::Error(e))
            if e.code.as_known() == Some(helper::HelperErrorCode::HELPER_ERROR_CODE_UNSUPPORTED)
    ));
    drop(s);

    let other = tempfile::tempdir().unwrap();
    let refused = started(other.path(), [u32::MAX - 1].into());
    assert!(refused.describe_storage().await.is_err(), "closed before a byte was read");
}
