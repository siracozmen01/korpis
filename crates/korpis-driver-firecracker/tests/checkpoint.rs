// SPDX-License-Identifier: AGPL-3.0-or-later

//! A guest checkpointed by one driver resumes under another, as a live migration moves it: what it
//! held only in memory is still there, it answers on its agent's channel, and the first is left
//! stopped. A state recorded as taken where this node does not restore it is refused, not booted.
//! Needs what the driver's runtime tests need.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

use korpis_driver_firecracker::machine::MachineError;
use korpis_proto::korpis::driver::v1 as driver;
use std::path::Path;
use std::time::Duration;

mod support;

use support::{IDS, Rig, live, ran, until};

/// Copies every file in `from` into `to`, made, as a migration's pass carries a volume.
fn carried(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

#[tokio::test]
async fn a_guest_checkpointed_by_one_driver_resumes_under_another_holding_its_memory() {
    if !live() {
        return;
    }
    let (source, destination) = (Rig::new(), Rig::new());
    let spec = source.spec("wl_moved", "while true; do sleep 1; done");
    let id = source.machine.create(spec.clone()).await.unwrap();
    source.machine.start(&id).await.unwrap();
    let answered = source.machine.ping_guest(&id, Duration::from_secs(60)).await.unwrap();
    assert_eq!(answered, Ok(()));
    let (_, end) = ran(&source.machine, &id, &["/bin/sh", "-c", "echo held > /tmp/mark"]).await;
    assert!(matches!(end, korpis_driver_kit::exec::Ran::Exit { status: 0, .. }), "{end:?}");

    let state = source.dir.path().join("volumes/wl_moved.live_state");
    std::fs::create_dir(&state).unwrap();
    let bytes = source.machine.checkpoint(&id, &state).await.unwrap();
    assert!(bytes > 128 << 20, "its memory, whole: {bytes}");
    let stopped = driver::RuntimeState::RUNTIME_STATE_STOPPED;
    until(&source.machine, &id, stopped, Duration::from_secs(20)).await;

    // The destination holds its own copy of the volume, under ids of its own, and of the state.
    let base = IDS + 1000 * 65_536;
    let volume = destination.volume("wl_moved", base);
    std::fs::copy(&spec.mounts[0].source, &volume).unwrap();
    let mut moved = spec.clone();
    moved.mounts[0].source = volume.to_string_lossy().into_owned();
    (moved.uid_base, moved.rootfs) = (base, destination.image.clone());
    let here = destination.dir.path().join("volumes/wl_moved.live_state");
    carried(&state, &here);
    // Another workload's state is not resumed, nor one recorded as taken where this node does not
    // restore it.
    let other = destination.dir.path().join("volumes/wl_other.live_state");
    carried(&state, &other);
    let id = destination.machine.create(moved).await.unwrap();
    let refused = destination.machine.resume(&id, &other).await;
    assert!(matches!(refused, Err(MachineError::Refused(_))), "not its own: {refused:?}");
    let record = here.join("state.json");
    let taken = std::fs::read_to_string(&record).unwrap();
    std::fs::write(&record, taken.replace("\"kernel\":\"", "\"kernel\":\"0.")).unwrap();
    let refused = destination.machine.resume(&id, &here).await;
    assert!(matches!(refused, Err(MachineError::Refused(_))), "{refused:?}");
    let running = Some(driver::RuntimeState::RUNTIME_STATE_RUNNING);
    assert_ne!(destination.machine.inspect(&id).unwrap().state.as_known(), running, "not booted");
    std::fs::write(&record, taken).unwrap();
    destination.machine.resume(&id, &here).await.unwrap();
    let answered = destination.machine.ping_guest(&id, Duration::from_secs(60)).await.unwrap();
    assert_eq!(answered, Ok(()), "its agent answers where it was resumed");
    let (said, _) = ran(&destination.machine, &id, &["/bin/cat", "/tmp/mark"]).await;
    assert_eq!(said, "held\n", "what it held in memory alone");
    destination.machine.destroy(&id).await.unwrap();
}
