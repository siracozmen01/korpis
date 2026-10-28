// SPDX-License-Identifier: AGPL-3.0-or-later

//! A migration's tree passes (§8.1 of docs/05-scheduling.md), for a volume in a class that sends
//! no increments: rsync, sending what differs between the source's volume and what the
//! destination holds, keeping owners by number, hardlinks, ACLs, extended attributes, and holes,
//! and deleting what the source no longer has.
//!
//! Each side speaks on one end of a socket pair whose other end the agent joins to the transfer
//! connection, so neither rsync opens a connection of its own. The receiving side is rsync's
//! daemon, run over its standard input and output as a remote shell would run it, with a
//! configuration of the helper's own that names one module, the volume, and confines the daemon
//! to it by its own `chroot`: the stream comes from another node, so what it may name is that
//! volume and nothing else. The sending side is rsync's client, whose remote shell is the
//! helper's own `splice` subcommand, joining the remote shell's standard input and output to the
//! socket: the one descriptor the helper hands a child without its close-on-exec flag, which it
//! closes itself as soon as the child holds it. The helper starts no thread, so no other child
//! is spawned while the flag is clear.

use crate::error::{HelperError, HelperResult, identifier};
use crate::storage::Class;
use rustix::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use rustix::io::FdFlags;
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// rsync: a requirement of a node offering a class that sends no increments for migration (§2
/// of docs/18-operations.md). Where it is missing the helper declares no `tree_sync`.
pub const RSYNC: &str = "/usr/bin/rsync";

/// Where a class keeps the receiving daemon's configurations.
const MIGRATING: &str = ".migrating";

/// The one module the receiving daemon offers.
const MODULE: &str = "tree";

/// Whether this node can send and receive trees.
#[must_use]
pub fn syncs_trees() -> bool {
    Path::new(RSYNC).is_file()
}

/// What a tree pass came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEnd {
    /// Still running.
    Running,
    /// Done; on the sending side, the bytes it sent.
    Done(u64),
    /// It ended short; why.
    Failed(String),
}

/// A socket pair, both ends closed on exec.
fn pair() -> HelperResult<(OwnedFd, OwnedFd)> {
    let made = socketpair(AddressFamily::UNIX, SocketType::STREAM, SocketFlags::CLOEXEC, None);
    Ok(made.map_err(std::io::Error::from)?)
}

/// The receiving daemon's configuration for a volume at `path`.
fn daemon_config(path: &Path) -> String {
    format!(
        "use chroot = yes\nnumeric ids = yes\nuid = 0\ngid = 0\nlist = no\n\n[{MODULE}]\n\
         path = {}\nread only = no\nmunge symlinks = no\n",
        path.display()
    )
}

/// The bytes the sending side's `--stats` said it sent, from its output.
fn sent_of(stats: &str) -> Option<u64> {
    let line = stats.lines().find_map(|l| l.trim().strip_prefix("Total bytes sent:"))?;
    line.trim().replace(',', "").parse().ok()
}

impl Class {
    /// The path of the daemon configuration for receiving into `volume`.
    fn tree_config(&self, volume: &str) -> PathBuf {
        self.root().join(MIGRATING).join(format!("{volume}.conf"))
    }

    /// Starts sending a pass of `volume`'s tree, with `relay` the helper's own binary, which
    /// joins rsync's remote shell to the socket: the socket's end the agent is given, and the
    /// sender.
    pub fn send_tree(&self, volume: &str, relay: &Path) -> HelperResult<(OwnedFd, Child)> {
        identifier("volume", volume)?;
        let source = self.path_of(volume);
        if !source.is_dir() {
            return Err(HelperError::NotFound(format!("volume {volume}")));
        }
        let (ours, theirs) = pair()?;
        rustix::io::fcntl_setfd(theirs.as_fd(), FdFlags::empty()).map_err(std::io::Error::from)?;
        let shell = format!("{} splice {}", relay.display(), theirs.as_raw_fd());
        let spawned = Command::new(RSYNC)
            .args(["-aHAXS", "--numeric-ids", "--delete", "--stats", "-e"])
            .arg(shell)
            .arg(format!("{}/", source.display()))
            .arg(format!("korpis::{MODULE}/"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn();
        // The child holds it now, or never will: either way the helper's copy goes.
        drop(theirs);
        Ok((ours, spawned?))
    }

    /// Starts receiving a pass into `volume`, which exists: the socket's end the agent is given,
    /// and the receiver.
    pub fn receive_tree(&self, volume: &str) -> HelperResult<(OwnedFd, Child)> {
        identifier("volume", volume)?;
        let into = self.path_of(volume);
        if !into.is_dir() {
            return Err(HelperError::NotFound(format!("volume {volume}")));
        }
        let config = self.tree_config(volume);
        if let Some(dir) = config.parent() {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        std::fs::write(&config, daemon_config(&into))?;
        let (ours, theirs) = pair()?;
        let child = Command::new(RSYNC)
            .args(["--server", "--daemon"])
            .arg(format!("--config={}", config.display()))
            .arg(".")
            .stdin(Stdio::from(theirs.try_clone()?))
            .stdout(Stdio::from(theirs))
            .spawn()?;
        Ok((ours, child))
    }

    /// What `child`, a pass of `volume`'s tree, came to; `sending` for the sending side, whose
    /// statistics name what it sent.
    pub fn tree_ended(
        &self,
        volume: &str,
        child: &mut Child,
        sending: bool,
    ) -> HelperResult<TreeEnd> {
        let Some(status) = child.try_wait()? else { return Ok(TreeEnd::Running) };
        if !sending {
            let _ = std::fs::remove_file(self.tree_config(volume));
        }
        if !status.success() {
            let why = format!("rsync ended with {status}; its errors are in the helper's log");
            tracing::warn!(%volume, sending, %why, "tree pass failed");
            return Ok(TreeEnd::Failed(why));
        }
        let mut said = String::new();
        if let Some(out) = child.stdout.as_mut() {
            out.read_to_string(&mut said)?;
        }
        let sent = if sending { sent_of(&said).unwrap_or(0) } else { 0 };
        tracing::info!(%volume, sending, sent, "tree pass done");
        Ok(TreeEnd::Done(sent))
    }
}

/// The `splice` subcommand: joins standard input and output to the socket `fd`, as rsync's
/// remote shell, until the far side has said all it will. rsync waits for its remote shell to
/// exit before it closes the shell's input, so the shell ends when the socket does rather than
/// when its input does.
///
/// # Errors
///
/// `fd` is not an open socket, or copying from it failed.
pub fn splice(fd: &str) -> std::io::Result<()> {
    let raw: i32 = fd.parse().map_err(|_| std::io::Error::other("not a descriptor"))?;
    let socket = crate::inherited::socket(raw)?;
    // Rust's standard output buffers by line; the protocol's bytes must not wait for a newline,
    // so both standard streams are used as the descriptors they are.
    let mut input = std::fs::File::from(std::io::stdin().as_fd().try_clone_to_owned()?);
    let mut output = std::fs::File::from(std::io::stdout().as_fd().try_clone_to_owned()?);
    let mut reading = std::fs::File::from(socket.try_clone()?);
    let mut writing = std::fs::File::from(socket);
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut input, &mut writing);
        let _ = rustix::net::shutdown(&writing, rustix::net::Shutdown::Write);
    });
    std::io::copy(&mut reading, &mut output)?;
    Ok(())
}
