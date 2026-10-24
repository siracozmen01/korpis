// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `firecracker` driver on this machine's KVM (§4.2 and §6 of docs/04-runtimes.md): a guest
//! created from an image in the content store runs its entry as its user, within its memory,
//! writing its volume, and its exit status is inspected; a stop reaches its entry and a kill ends
//! one that ignores it; a restarted driver finds its guests; and a spec outside what the driver
//! builds is refused. Needs root, /dev/kvm, Firecracker and its jailer (scripts/tools.sh), the
//! guest kernel (scripts/guest-kernel.sh), mkfs.ext4 and debugfs, and is skipped without them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_driver_firecracker::machine::{Machine, MachineError, START_LOG};
use korpis_driver_kit::exec::Ran;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::process::Command;
use std::time::Duration;

mod support;

use support::{MIB, Rig, live, ran, until};

fn debugfs(drive: &str, request: &str) -> String {
    let out = Command::new("debugfs").args(["-R", request, drive]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn a_guest_runs_as_its_user_within_its_memory_and_its_exit_status_is_inspected() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let script = "{ id -u; grep MemTotal /proc/meminfo; } > /data/seen; sleep 2; exit 7";
    let spec = rig.spec("wl_runs", script);
    let id = rig.machine.create(spec.clone()).await.unwrap();
    assert_eq!(rig.machine.create(spec.clone()).await.unwrap(), id, "one object per intent");
    let created = rig.machine.inspect(&id).unwrap();
    assert_eq!(created.state.as_known(), Some(driver::RuntimeState::RUNTIME_STATE_CREATED));
    rig.machine.start(&id).await.unwrap();
    until(&rig.machine, &id, driver::RuntimeState::RUNTIME_STATE_RUNNING, Duration::from_secs(5))
        .await;
    let cgroup = rig.machine.settings().cgroup_of(&id);
    let max = std::fs::read_to_string(cgroup.join("memory.max")).unwrap();
    assert_eq!(max.trim(), (192 * MIB).to_string(), "the guest's memory and its Firecracker's");
    let stopped = until(
        &rig.machine,
        &id,
        driver::RuntimeState::RUNTIME_STATE_STOPPED,
        Duration::from_secs(30),
    )
    .await;
    let said = std::fs::read_to_string(rig.machine.settings().object(&id).join(START_LOG));
    assert_eq!(stopped.exit_code, Some(7), "{stopped:?}, the jailer said {said:?}");
    let seen = debugfs(&spec.mounts[0].source, "cat /seen");
    assert!(seen.starts_with("1000\n"), "as its user: {seen}");
    let kib: u64 = seen.split_whitespace().nth(2).unwrap().parse().unwrap();
    assert!(kib <= 128 * 1024 && kib > 64 * 1024, "within its 128 MiB: {kib} kB");
    // A second run, in a jail built afresh: nothing the first left in its chroot is read.
    rig.machine.start(&id).await.unwrap();
    let again = until(
        &rig.machine,
        &id,
        driver::RuntimeState::RUNTIME_STATE_STOPPED,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(again.exit_code, Some(7), "{again:?}");
    rig.machine.destroy(&id).await.unwrap();
    let s = rig.machine.settings();
    assert!(!s.object(&id).exists() && !s.jail(&id).exists() && !cgroup.exists());
}

#[tokio::test]
async fn a_stop_and_a_signal_named_reach_the_entry_and_a_kill_ends_one_that_ignores_a_stop() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let heeds = rig.spec("wl_heeds", "trap 'sleep 2; exit 3' TERM; while true; do sleep 1; done");
    let ignores = rig.spec(
        "wl_ignores",
        "trap '' TERM; trap 'touch /tmp/hup' HUP; while true; do sleep 1; done",
    );
    let (a, b) =
        (rig.machine.create(heeds).await.unwrap(), rig.machine.create(ignores).await.unwrap());
    let mut events = rig.machine.events();
    for id in [&a, &b] {
        rig.machine.start(id).await.unwrap();
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let answered = rig.machine.ping_guest(&a, Duration::from_secs(5)).await.unwrap();
    assert_eq!(answered, Ok(()), "the guest's agent answers on its control port");
    let (said, end) = ran(&rig.machine, &a, &["/bin/sh", "-c", "id -u; exit 2"]).await;
    assert_eq!(
        (said.as_str(), end),
        ("1000\n", Ran::Exit { status: 2, timed_out: false, truncated: false })
    );
    rig.machine.stop(&a, "", Duration::from_secs(30)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    // As an agent restarted in the middle of a stop asks again: still its stop, not its kill.
    rig.machine.stop(&a, "", Duration::from_secs(30)).await.unwrap();
    let s = until(
        &rig.machine,
        &a,
        driver::RuntimeState::RUNTIME_STATE_STOPPED,
        Duration::from_secs(20),
    )
    .await;
    assert_eq!(s.exit_code, Some(3), "the entry's own exit, on SIGTERM, asked twice: {s:?}");
    rig.machine.signal(&b, "SIGHUP").await.unwrap();
    let unknown = rig.machine.signal(&b, "SIGSEGV").await;
    assert!(matches!(unknown, Err(MachineError::Refused(_))), "only the signals it declares");
    rig.machine.kill(&b).await.unwrap();
    let s = until(
        &rig.machine,
        &b,
        driver::RuntimeState::RUNTIME_STATE_STOPPED,
        Duration::from_secs(20),
    )
    .await;
    assert_eq!(s.exit_code, Some(137), "ended by the second stop: {s:?}");
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv()).await.unwrap().unwrap();
    assert!([&a, &b].contains(&&event.runtime_id), "each start and end is an event");
    for id in [&a, &b] {
        rig.machine.destroy(id).await.unwrap();
    }
}

#[tokio::test]
async fn a_restarted_driver_finds_its_guests_and_a_spec_outside_its_tier_is_refused() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let stays = rig.spec("wl_stays", "while true; do sleep 1; done");
    let mut shares = rig.spec("wl_shares", "true");
    shares.mounts[0].source.clone_from(&stays.mounts[0].source);
    let id = rig.machine.create(stays).await.unwrap();
    let refused_share = rig.machine.create(shares).await;
    assert!(
        matches!(refused_share, Err(MachineError::Refused(_))),
        "one workload's at a time: {refused_share:?}"
    );
    rig.machine.start(&id).await.unwrap();
    let again = Machine::new(rig.machine.settings().clone()).unwrap();
    let found = again.recover();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].labels.as_option().unwrap().workload, "wl_stays");
    let running = driver::RuntimeState::RUNTIME_STATE_RUNNING;
    assert_eq!(found[0].status.as_option().unwrap().state.as_known(), Some(running));
    again.kill(&id).await.unwrap();
    until(&again, &id, driver::RuntimeState::RUNTIME_STATE_STOPPED, Duration::from_secs(20)).await;
    again.destroy(&id).await.unwrap();

    let refused = |mut s: driver::PreparedSpec, how: &dyn Fn(&mut driver::PreparedSpec)| {
        how(&mut s);
        s
    };
    let base = rig.spec("wl_refused", "true");
    let other = rig.machine.settings().identity_root.join("wl_other");
    let cases: [&dyn Fn(&mut driver::PreparedSpec); 6] = [
        &|s| s.tier = pb::IsolationTier::ISOLATION_TIER_CONTAINER.into(),
        &|s| s.uid_base = 0,
        &|s| s.namespaces = "/run/korpis/netns/x".into(),
        &|s| s.mounts[0].source = "/etc/passwd".into(),
        &|s| s.identity_dir = other.to_string_lossy().into_owned(),
        &|s| s.mounts[0].destination = "/run/korpis/config".into(),
    ];
    for how in cases {
        let made = rig.machine.create(refused(base.clone(), how)).await;
        assert!(matches!(made, Err(MachineError::Refused(_))), "{made:?}");
    }
}

#[tokio::test]
async fn a_guest_boots_the_node_s_kernel_and_boot_drive_as_they_are_at_its_start() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let id = rig.machine.create(rig.spec("wl_upgraded", "exit 0")).await.unwrap();
    // As an object made before the node's kernel and korpis-guest were upgraded: its record
    // names drives the node no longer has.
    let record = rig.machine.settings().object(&id).join("drives.json");
    let mut drives: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    let current = (drives["kernel"].clone(), drives["boot"].clone());
    drives["kernel"] = "/nonexistent/vmlinux-old".into();
    drives["boot"] = "/nonexistent/boot-old.ext4".into();
    std::fs::write(&record, serde_json::to_vec(&drives).unwrap()).unwrap();

    rig.machine.start(&id).await.unwrap();
    let s = until(
        &rig.machine,
        &id,
        driver::RuntimeState::RUNTIME_STATE_STOPPED,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(s.exit_code, Some(0), "it booted");
    let kept: serde_json::Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    assert_eq!((kept["kernel"].clone(), kept["boot"].clone()), current, "the node's, now");
    rig.machine.destroy(&id).await.unwrap();
}
