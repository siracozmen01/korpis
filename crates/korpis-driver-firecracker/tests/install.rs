// SPDX-License-Identifier: AGPL-3.0-or-later

//! An install into a stopped guest's volume (§4.4 of docs/04-runtimes.md, §4 of
//! docs/09-recipes.md): the driver boots a service guest with the install's content drive, the
//! guest runs the install job its boot drive carries, confined as on a host, and the driver keeps
//! what it wrote; the workload's start is refused while it runs; an install asked to start from a
//! later action skips the earlier ones; and the workload, started, reads what was written. Needs
//! what the driver's runtime tests need.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failing test should fail loudly"
)]

use korpis_core::recipe::install::InstallStep;
use korpis_core::recipe::installing::{InstallAction, detail};
use korpis_driver_firecracker::machine::{Install, InstallStatus, Machine, MachineError};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

mod support;

use support::{Rig, live, no_service_guest, ran, until};

/// Keeps `bytes` among the recipe files of the content store at `content`; their digest.
fn keep(content: &Path, bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let hex = sha2::Sha256::digest(bytes).iter().fold(String::new(), |mut s, b| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    });
    let blobs = content.join("blobs/sha256");
    std::fs::create_dir_all(&blobs).unwrap();
    std::fs::write(blobs.join(&hex), bytes).unwrap();
    format!("sha256:{hex}")
}

fn act(step: InstallStep) -> Value {
    korpis_wire::value::to_pb(&detail::encode(&InstallAction { step, context: None }))
}

fn write(path: &str, content: &str) -> Value {
    act(InstallStep::Write { path: path.into(), content: content.into(), mode: 0o644 })
}

/// How install `job` stands once it no longer runs.
async fn ended(machine: &Machine, job: &str) -> InstallStatus {
    for _ in 0..600 {
        let s = machine.install_status(job).await;
        if s.state != driver::InstallState::INSTALL_STATE_RUNNING {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("install {job} still runs");
}

#[tokio::test]
async fn an_install_writes_into_a_stopped_guest_s_volume_confined_and_resumes() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let spec = rig.spec("wl_inst", "while true; do sleep 1; done");
    let motd = keep(&rig.machine.settings().content, b"hos geldin\n");
    let copy = act(InstallStep::Copy { from: "motd.txt".into(), to: "/data/motd.txt".into() });
    let first = Install {
        job: "ins-first".into(),
        spec: spec.clone(),
        actions: vec![write("/data/a.txt", "a"), copy, write("/data/b.txt", "b")],
        from: 0,
        files: [("motd.txt".to_owned(), motd)].into(),
        fetched: Vec::new(),
    };
    // Made first, since making it builds the image's drive, which outlasts a short install.
    let id = rig.machine.create(spec.clone()).await.unwrap();
    rig.machine.start_install(first.clone()).await.unwrap();
    rig.machine.start_install(first).await.unwrap();
    let refused = rig.machine.start(&id).await;
    assert!(matches!(refused, Err(MachineError::Refused(_))), "a start waits for the install");

    let s = ended(&rig.machine, "ins-first").await;
    assert_eq!(s.state, driver::InstallState::INSTALL_STATE_FINISHED);
    let result = s.result.unwrap();
    assert!(result.ok, "{}", result.error);
    assert_eq!((result.done, s.progress), (3, 3));
    let confined = result.confinement.into_option().unwrap();
    assert!(confined.namespaces && confined.landlock && confined.own_ids, "{confined:?}");
    no_service_guest(&rig.machine).await;

    let resumed = Install {
        job: "ins-resumed".into(),
        spec,
        actions: vec![write("/data/a.txt", "never"), write("/data/c.txt", "c")],
        from: 1,
        files: BTreeMap::default(),
        fetched: Vec::new(),
    };
    rig.machine.start_install(resumed).await.unwrap();
    let s = ended(&rig.machine, "ins-resumed").await;
    assert!(s.result.unwrap().ok);
    no_service_guest(&rig.machine).await;
    rig.machine.forget_install("ins-first").await;
    let forgotten = rig.machine.install_status("ins-first").await.state;
    assert_eq!(forgotten, driver::InstallState::INSTALL_STATE_UNKNOWN);

    rig.machine.start(&id).await.unwrap();
    let running = driver::RuntimeState::RUNTIME_STATE_RUNNING;
    until(&rig.machine, &id, running, Duration::from_secs(30)).await;
    let (said, _) = ran(&rig.machine, &id, &["/bin/cat", "/data/motd.txt", "/data/a.txt"]).await;
    assert_eq!(said, "hos geldin\na", "the recipe's file, and the first action's, not the skipped");
    let (said, _) = ran(&rig.machine, &id, &["/bin/cat", "/data/b.txt", "/data/c.txt"]).await;
    assert_eq!(said, "bc");
    rig.machine.destroy(&id).await.unwrap();
}
