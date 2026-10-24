// SPDX-License-Identifier: AGPL-3.0-or-later

//! A microVM's `/run/korpis` (§9.5 of docs/08-identity.md, §6 of docs/04-runtimes.md): what the
//! workload says on its identity socket inside its guest reaches its own socket on the host, and
//! is answered; the API relay is served inside once the node serves it on the host, and withdrawn
//! with it; a restarted driver serves a running guest's relays again; and an identity directory
//! that resolves to another workload's is refused at the start. Needs what the driver's runtime
//! tests need.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_driver_firecracker::machine::{Machine, MachineError};
use korpis_driver_kit::exec::Ran;
use korpis_proto::korpis::driver::v1 as driver;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::{Duration, Instant};

mod support;

use support::{Rig, example, live, ran, until};

const CLIENT: &str = "/bin/unix-client";

/// Answers each line said on a socket at `path` with `name` and the line, in a thread of its own,
/// as a node's identity socket or API relay would answer the workload's.
fn answering(path: &Path, name: &'static str) {
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_ok() {
                let _ = (&stream).write_all(format!("{name} {line}").as_bytes());
            }
        }
    });
}

/// What the guest of `id` is answered on `/run/korpis/<socket>`, and how its client ended.
async fn asked(machine: &Machine, id: &str, socket: &str) -> (String, Ran) {
    ran(machine, id, &[CLIENT, &format!("/run/korpis/{socket}"), "hello"]).await
}

/// Waits, within twenty seconds, until `id`'s guest is answered `want` on `socket`.
async fn answered(machine: &Machine, id: &str, socket: &str, want: &str) {
    let start = Instant::now();
    loop {
        let (said, _) = asked(machine, id, socket).await;
        if said == want {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "{socket} answered {said:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

const SUCCEEDED: Ran = Ran::Exit { status: 0, timed_out: false, truncated: false };
const FAILED: Ran = Ran::Exit { status: 1, timed_out: false, truncated: false };

#[tokio::test]
async fn a_guest_s_run_korpis_reaches_the_workload_s_own_sockets_on_its_host() {
    if !live() {
        return;
    }
    let client = std::fs::read(example("korpis-guest", "unix-client")).unwrap();
    let rig = Rig::with(&[("bin/unix-client", &client)]);
    let root = rig.machine.settings().identity_root.clone();
    let own = root.join("wl_said");
    std::fs::create_dir_all(&own).unwrap();
    answering(&own.join("identity.sock"), "identity");
    let mut spec = rig.spec("wl_said", "while true; do sleep 1; done");
    spec.identity_dir = own.to_string_lossy().into_owned();
    let id = rig.machine.create(spec).await.unwrap();
    rig.machine.start(&id).await.unwrap();
    let running = driver::RuntimeState::RUNTIME_STATE_RUNNING;
    until(&rig.machine, &id, running, Duration::from_secs(30)).await;

    assert_eq!(
        asked(&rig.machine, &id, "identity.sock").await,
        ("identity hello\n".into(), SUCCEEDED)
    );
    let (_, none) = asked(&rig.machine, &id, "api.sock").await;
    assert_eq!(none, FAILED, "no API relay until the node serves one");
    answering(&own.join("api.sock"), "api");
    answered(&rig.machine, &id, "api.sock", "api hello\n").await;

    // A restarted driver serves the running guest's relays again.
    let again = Machine::new(rig.machine.settings().clone()).unwrap();
    assert_eq!(again.recover().len(), 1);
    answered(&again, &id, "identity.sock", "identity hello\n").await;
    std::fs::remove_file(own.join("api.sock")).unwrap();
    let start = Instant::now();
    while ran(&again, &id, &["/bin/sh", "-c", "test -e /run/korpis/api.sock"]).await.1 != FAILED {
        assert!(start.elapsed() < Duration::from_secs(20), "the API relay is withdrawn");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    again.kill(&id).await.unwrap();
    until(&again, &id, driver::RuntimeState::RUNTIME_STATE_STOPPED, Duration::from_secs(20)).await;

    // The directory now resolves to another workload's: the start is refused.
    let other = root.join("wl_other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::remove_dir_all(&own).unwrap();
    std::os::unix::fs::symlink(&other, &own).unwrap();
    let started = again.start(&id).await;
    assert!(matches!(started, Err(MachineError::Refused(_))), "{started:?}");
    again.destroy(&id).await.unwrap();
}
