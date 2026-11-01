// SPDX-License-Identifier: AGPL-3.0-or-later

//! The view a tree's receiving rsync runs in: a program in it runs, finds its libraries, writes
//! into the copy at its own path and gives a file an owner by number, sees nothing else of the
//! host, its `/etc/hostname` and the parent of the copy absent, cannot mount, which is how root
//! would leave the view, and cannot signal the host's root processes, since it is not root. It
//! needs root, and says so and does not run without it.

#![allow(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

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
        eprintln!("needs root and /etc/hostname: not run");
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
    assert!(inside(&copy, &format!("chown 4321:4321 {c}/written")), "owners are kept by number");
    let mounted = inside(&copy, &format!("mount -t tmpfs none {c}"));
    assert!(!mounted, "nothing in it mounts, so it cannot reach the host through a proc mount");
    let mut host = Command::new("sleep").arg("60").spawn().unwrap();
    let signalled = inside(&copy, &format!("kill -0 {}", host.id()));
    host.kill().unwrap();
    host.wait().unwrap();
    assert!(!signalled, "it is not root, so it signals none of the host's root processes");
}
