// SPDX-License-Identifier: AGPL-3.0-or-later

//! Confinement as the kernel enforces it. The test runs itself again as a child, which confines
//! itself as a job does and then tries what a job must not be able to: read a file outside its
//! directory, write into a backup's source, connect to a port that is not the store's, and keep
//! a capability its work does not need. The child reports what it could do and the parent
//! checks it, so a confinement that silently does nothing fails the test.

use korpis_backup::confine::{Bounds, Work, confine};
use std::path::PathBuf;
use std::process::Command;

const CHILD: &str = "KORPIS_CONFINE_CHILD";

/// What the confined child tries, each as `name=yes|no`.
fn probe(dir: &std::path::Path, outside: &std::path::Path, open_port: u16) -> String {
    let read_inside = std::fs::read(dir.join("inside")).is_ok();
    let read_outside = std::fs::read(outside).is_ok();
    let write_inside = std::fs::write(dir.join("new"), b"x").is_ok();
    let connect_other = std::net::TcpStream::connect(("127.0.0.1", open_port)).is_ok();
    let caps = rustix::thread::capabilities(None)
        .map_or(rustix::thread::CapabilitySet::all(), |c| c.effective);
    let only = caps == rustix::thread::CapabilitySet::DAC_READ_SEARCH;
    format!(
        "read_inside={read_inside} read_outside={read_outside} write_inside={write_inside} connect_other={connect_other} only_dac_read_search={only}"
    )
}

fn child() {
    let dir = PathBuf::from(std::env::var("KORPIS_DIR").unwrap());
    let state = PathBuf::from(std::env::var("KORPIS_STATE").unwrap());
    let outside = PathBuf::from(std::env::var("KORPIS_OUTSIDE").unwrap());
    let open_port: u16 = std::env::var("KORPIS_OPEN_PORT").unwrap().parse().unwrap();
    let b = Bounds {
        work: Work::Backup,
        directory: dir.clone(),
        state: state.clone(),
        content: None,
        cgroup: None,
        port: 9,
    };
    let held = confine(&b).unwrap();
    assert!(held.landlock_filesystem && held.landlock_network, "{held:?}");
    let report = probe(&dir, &outside, open_port);
    std::fs::write(state.join("report"), report).unwrap();
}

#[test]
fn a_confined_job_reaches_only_its_directory_and_its_store() {
    if std::env::var_os(CHILD).is_some() {
        return child();
    }
    if !rustix::process::geteuid().is_root() {
        eprintln!("confinement drops capabilities, which needs root: not run");
        return;
    }
    let tmp = env!("CARGO_TARGET_TMPDIR");
    let (dir, state, other) = (
        tempfile::tempdir_in(tmp).unwrap(),
        tempfile::tempdir_in(tmp).unwrap(),
        tempfile::tempdir_in(tmp).unwrap(),
    );
    std::fs::write(dir.path().join("inside"), b"tenant data").unwrap();
    let outside = other.path().join("secret");
    std::fs::write(&outside, b"host data").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "confine::a_confined_job_reaches_only_its_directory_and_its_store"])
        .env(CHILD, "1")
        .env("KORPIS_DIR", dir.path())
        .env("KORPIS_STATE", state.path())
        .env("KORPIS_OUTSIDE", &outside)
        .env("KORPIS_OPEN_PORT", port.to_string())
        .status()
        .unwrap();
    assert!(status.success());
    let report = std::fs::read_to_string(state.path().join("report")).unwrap();
    assert_eq!(
        report,
        "read_inside=true read_outside=false write_inside=false connect_other=false only_dac_read_search=true"
    );
}
