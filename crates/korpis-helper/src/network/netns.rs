// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's namespaces, kept by bind mounts as `ip netns` keeps a network namespace: a
//! namespace lives while a file is mounted over it, so it outlives the process that made it and
//! is joined by its path (§4.1 of docs/07-networking.md). Each workload has a directory holding
//! two: `net`, its network, and `user`, the user namespace that owns it, mapped to the
//! workload's uid range as its container's is. The container joins both, because the kernel lets
//! a container's root mount its own sysfs only in a network namespace its user namespace owns;
//! its capabilities leave out `CAP_NET_ADMIN`, so owning the namespace does not let it change it.
//!
//! The pair is made by a child that unshares both and waits while the helper writes its id
//! maps and binds its namespaces, because a user namespace can only be made by a process with
//! one thread. The helper's own work inside a namespace is done by its one thread entering it
//! and returning to the host's, which it holds open from before it left; a return that fails
//! leaves the helper where it must not stay, and it exits rather than serve another request.
//!
//! This is one of the crate's unsafe modules: `unshare(2)` has no safe wrapper, and it runs in
//! the child between fork and exec.

#![expect(unsafe_code, reason = "unshare(2) has no safe wrapper, and runs before exec")]

use rustix::fs::{Mode, OFlags};
use rustix::mount::{UnmountFlags, mount_bind, unmount};
use rustix::thread::{LinkNameSpaceType, UnshareFlags, move_into_link_name_space, unshare_unsafe};
use std::io;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// What `nsfs` reports as its magic number, which a bound namespace file has.
const NSFS_MAGIC: rustix::fs::FsWord = 0x6e73_6673;

fn open(path: impl rustix::path::Arg) -> io::Result<OwnedFd> {
    Ok(rustix::fs::open(path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())?)
}

/// Whether `path` is a namespace bound there.
fn bound(path: &Path) -> bool {
    open(path).is_ok_and(|fd| rustix::fs::fstatfs(&fd).is_ok_and(|s| s.f_type == NSFS_MAGIC))
}

/// The network namespace in `dir`.
pub(crate) fn net(dir: &Path) -> std::path::PathBuf {
    dir.join("net")
}

/// What `dir` records of the id range its user namespace maps.
fn mapped(base: u32, count: u32) -> String {
    format!("{base} {count}\n")
}

/// Makes the namespaces in `dir` for the uid range at `base` of `count`, unless they are there
/// and map that range; whether it made them.
pub(crate) fn ensure(dir: &Path, (base, count): (u32, u32)) -> io::Result<bool> {
    let ids = dir.join("ids");
    let kept = std::fs::read_to_string(&ids).ok();
    if bound(&net(dir)) && bound(&dir.join("user")) && kept == Some(mapped(base, count)) {
        return Ok(false);
    }
    remove(dir)?;
    std::fs::create_dir_all(dir)?;
    for name in ["net", "user"] {
        std::fs::File::create(dir.join(name))?;
    }
    let mut command = Command::new("cat");
    command.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: the closure runs in the forked child before exec and only makes one system call,
    // which allocates nothing and takes no lock, as a forked child of a threaded process must.
    // `CLONE_FILES` is not asked for, so no file descriptor table is changed.
    unsafe {
        command.pre_exec(|| Ok(unshare_unsafe(UnshareFlags::NEWUSER | UnshareFlags::NEWNET)?));
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    let made = (|| -> io::Result<()> {
        let map = format!("0 {base} {count}\n");
        std::fs::write(format!("/proc/{pid}/uid_map"), &map)?;
        std::fs::write(format!("/proc/{pid}/gid_map"), &map)?;
        mount_bind(format!("/proc/{pid}/ns/user"), dir.join("user"))?;
        mount_bind(format!("/proc/{pid}/ns/net"), net(dir))?;
        // Kept in the runtime directory, as the namespaces are: a reboot loses both.
        std::fs::write(&ids, mapped(base, count))
    })();
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    made?;
    Ok(true)
}

/// Removes the namespaces in `dir` and `dir` itself; whether any was there. A namespace itself
/// ends with the last process in it.
pub(crate) fn remove(dir: &Path) -> io::Result<bool> {
    let mut was = false;
    for name in ["net", "user"] {
        let path = dir.join(name);
        if bound(&path) {
            unmount(&path, UnmountFlags::DETACH)?;
            was = true;
        }
    }
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(was),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(was),
        Err(e) => Err(e),
    }
}

/// Runs `work` inside the network namespace bound at `path`, returning to the host's after, or
/// exiting the process if it cannot.
pub(crate) fn within<T>(path: &Path, work: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let target = open(path)?;
    let home = open("/proc/thread-self/ns/net")?;
    move_into_link_name_space(target.as_fd(), Some(LinkNameSpaceType::Network))?;
    let done = work();
    if let Err(e) = move_into_link_name_space(home.as_fd(), Some(LinkNameSpaceType::Network)) {
        tracing::error!(error = %e, "the helper could not return to the host's network namespace");
        std::process::exit(70);
    }
    done
}
