// SPDX-License-Identifier: AGPL-3.0-or-later

//! A guest's volume as a block device (§3.1 of docs/06-storage.md): the drive is its guest's
//! filesystem, which the guest's own kernel grows to fill the drive when the drive has been made
//! larger between runs, as the helper makes it when a workload is given more; nothing on the host
//! touches the filesystem. Needs what the runtime tests need.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod support;

use korpis_driver_firecracker::machine::Machine;
use korpis_proto::korpis::driver::v1 as driver;
use std::process::Command;
use std::time::{Duration, Instant};
use support::{MIB, Rig, live};

async fn ended(m: &Machine, id: &str) {
    let start = Instant::now();
    let stopped = Some(driver::RuntimeState::RUNTIME_STATE_STOPPED);
    while m.inspect(id).unwrap().state.as_known() != stopped {
        assert!(start.elapsed() < Duration::from_secs(30), "{id} never ended");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The size in KiB the guest's last `df` said its volume at `/data` holds.
fn said_size(drive: &str) -> u64 {
    let out = Command::new("debugfs").args(["-R", "cat /size", drive]).output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    let total = said.split_whitespace().nth(1).unwrap_or_default();
    total.parse().unwrap_or_else(|_| panic!("the guest said {said:?}"))
}

#[tokio::test]
async fn a_volume_made_larger_is_grown_by_its_guest() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let spec = rig.spec("wl_grows", "df -k /data | tail -1 > /data/size");
    let drive = spec.mounts[0].source.clone();
    let id = rig.machine.create(spec).await.unwrap();
    rig.machine.start(&id).await.unwrap();
    ended(&rig.machine, &id).await;
    let before = said_size(&drive);
    assert!(before < 16 * 1024, "{before} KiB on a 16 MiB drive");

    let image = std::fs::OpenOptions::new().write(true).open(&drive).unwrap();
    image.set_len(64 * MIB).unwrap();
    rig.machine.start(&id).await.unwrap();
    ended(&rig.machine, &id).await;
    let after = said_size(&drive);
    assert!(after > 48 * 1024, "grown to fill its 64 MiB drive: {before} KiB, then {after} KiB");
    rig.machine.destroy(&id).await.unwrap();
}
