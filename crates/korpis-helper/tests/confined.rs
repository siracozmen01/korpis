// SPDX-License-Identifier: AGPL-3.0-or-later

//! The view a tree's receiving rsync runs in (Finding 218): a program in it runs, finds its
//! libraries, and writes into the copy at its own path, and sees nothing else of the host, its
//! `/etc/hostname` and the parent of the copy absent. It needs root, and is skipped without it.

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use korpis_helper::confined::confine;
use std::path::Path;
use std::process::Command;

/// Whether `script`, run by the shell in the view holding `copy`, succeeds.
fn inside(copy: &Path, script: &str) -> bool {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    confine(&mut command, copy, Path::new("/bin/sh")).unwrap();
    command.status().unwrap().success()
}

#[test]
fn a_confined_program_sees_its_copy_and_nothing_else_of_the_host() {
    if !rustix::process::geteuid().is_root() || !Path::new("/etc/hostname").exists() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("copy");
    std::fs::create_dir(&copy).unwrap();
    std::fs::write(dir.path().join("beside"), "not the copy's").unwrap();
    let c = copy.display();
    assert!(inside(&copy, &format!("echo kept > {c}/written")), "it runs and writes its copy");
    assert_eq!(std::fs::read_to_string(copy.join("written")).unwrap(), "kept\n");
    assert!(!inside(&copy, "test -e /etc/hostname"), "the host's files are not in it");
    let beside = dir.path().join("beside");
    assert!(!inside(&copy, &format!("test -e {}", beside.display())), "nor the copy's parent's");
    assert!(!inside(&copy, "touch /usr/korpis-written"), "and its programs are read only");
}
