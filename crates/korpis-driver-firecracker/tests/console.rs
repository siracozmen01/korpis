// SPDX-License-Identifier: AGPL-3.0-or-later

//! A guest's console (§6 of docs/04-runtimes.md; §4 and §6 of docs/14-streams.md): what a guest
//! of logs prints reaches its stream as it printed it, with no carriage return a terminal would
//! add, and takes no input; what is typed into an interactive console reaches its entry through
//! its terminal, recorded with who typed it; and the writer ends with its guest. Needs what the
//! runtime tests need.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod support;

use korpis_driver_firecracker::machine::{Machine, MachineError};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_stream::dir::StreamDir;
use korpis_stream::read::read;
use std::path::Path;
use std::time::{Duration, Instant};
use support::{Rig, live};

/// The bodies of the stream in `dir`, once one satisfies `done`.
async fn bodies_until(dir: &Path, done: impl Fn(&[String]) -> bool) -> Vec<String> {
    let start = Instant::now();
    loop {
        let records = read(&StreamDir::new(dir), 0, 1000).map(|b| b.records).unwrap_or_default();
        let bodies: Vec<String> =
            records.iter().map(|r| String::from_utf8_lossy(&r.body).into_owned()).collect();
        if done(&bodies) {
            return bodies;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "{} held {bodies:?}", dir.display());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn until(m: &Machine, id: &str, state: driver::RuntimeState) {
    let start = Instant::now();
    while m.inspect(id).unwrap().state.as_known() != Some(state) {
        assert!(start.elapsed() < Duration::from_secs(30), "{id} never reached {state:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_console_of_logs_holds_what_the_guest_printed_and_takes_no_input() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let (spec, dir) = rig.console(rig.spec("wl_logs", "echo one; echo two >&2; exit 0"), false);
    let id = rig.machine.create(spec).await.unwrap();
    rig.machine.start(&id).await.unwrap();
    let said = bodies_until(&dir, |b| b.iter().any(|l| l == "two\n")).await;
    assert!(said.iter().any(|l| l == "one\n"), "as printed, no carriage return: {said:?}");
    let typed = rig.machine.console_input(&id, ("user:alex", "s1"), b"x\n").await;
    assert!(matches!(typed, Err(MachineError::Refused(_))), "{typed:?}");
    until(&rig.machine, &id, driver::RuntimeState::RUNTIME_STATE_STOPPED).await;
    rig.machine.destroy(&id).await.unwrap();
    assert!(!rig.machine.settings().cgroup_of(&id).exists(), "the writer left with its guest");
}

#[tokio::test]
async fn an_interactive_console_takes_input_through_the_guest_s_terminal() {
    if !live() {
        return;
    }
    let rig = Rig::new();
    let script = "while read line; do echo got $line $(stty size); done";
    let (spec, dir) = rig.console(rig.spec("wl_talks", script), true);
    let id = rig.machine.create(spec).await.unwrap();
    rig.machine.start(&id).await.unwrap();
    until(&rig.machine, &id, driver::RuntimeState::RUNTIME_STATE_RUNNING).await;
    rig.machine.console_input(&id, ("user:alex", "s1"), b"hello\n").await.unwrap();
    let said = bodies_until(&dir, |b| b.iter().any(|l| l.contains("got hello"))).await;
    let typed = said.iter().find(|b| b.contains("\"text\"")).expect("the input, recorded");
    let typed: serde_json::Value = serde_json::from_str(typed).unwrap();
    assert_eq!(
        (typed["by"].as_str(), typed["text"].as_str()),
        (Some("user:alex"), Some("hello\n"))
    );
    // A resize is recorded with who made it, and the entry's terminal has the size.
    rig.machine.resize_console(&id, ("user:alex", "s1"), (100, 30)).await.unwrap();
    rig.machine.console_input(&id, ("user:alex", "s1"), b"again\n").await.unwrap();
    let said = bodies_until(&dir, |b| b.iter().any(|l| l.contains("got again"))).await;
    assert!(said.iter().any(|l| l.contains("got again 30 100")), "the size it was given: {said:?}");
    assert!(said.iter().any(|b| b.contains("\"resize\":[100,30]")), "recorded: {said:?}");
    rig.machine.kill(&id).await.unwrap();
    until(&rig.machine, &id, driver::RuntimeState::RUNTIME_STATE_STOPPED).await;
    rig.machine.destroy(&id).await.unwrap();
}
