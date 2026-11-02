// SPDX-License-Identifier: AGPL-3.0-or-later

//! Replicas, in a class that can send increments, which is btrfs. A volume's node keeps the
//! read-only snapshots it sent under the class root's `.replicas/<volume>/`, a directory no volume
//! identifier can name, and a replica's node keeps there the ones it received, each a subvolume
//! made from the stream. An increment is the difference from a snapshot both sides hold.
//!
//! Sending is the kernel's own `BTRFS_IOC_SEND`, writing into a pipe whose read end the agent is
//! given, on a thread of its own while the agent reads. Receiving is btrfs-progs' `btrfs
//! receive`, confined by its own `chroot` to the volume's replica directory, reading a pipe
//! whose write end the agent is given; the stream comes from another node, so what it may name is
//! that directory and nothing else. A snapshot is held whole once `btrfs receive` made it
//! read-only, which it does at the stream's end alone; anything else of a receive that failed is
//! removed. A replica becomes the volume as a writable snapshot of its last snapshot, made where
//! the volume would be.

use crate::btrfs;
use crate::error::{HelperError, HelperResult, identifier};
use crate::snapshot::is_subvolume;
use crate::storage::{Backend, Class};
use rustix::fd::OwnedFd;
use std::fs::File;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Where a class keeps what its volumes' replication holds.
const REPLICAS: &str = ".replicas";

/// btrfs-progs, whose `receive` applies a stream: a requirement of a node holding replicas.
pub const BTRFS: &str = "/usr/bin/btrfs";

/// What a receive came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// Still reading the stream.
    Running,
    /// The snapshot is held here whole.
    Held,
    /// The stream ended short or was refused, and nothing of it is kept; why.
    Failed(String),
}

/// Whether the subvolume at `path` exists and is read-only, as a snapshot received whole is.
fn held_at(path: &Path) -> HelperResult<bool> {
    Ok(is_subvolume(path)? && btrfs::read_only(&File::open(path)?)?)
}

impl Class {
    /// The pass `pass` of `volume`'s replication here, held whole: a migration's pass, on the
    /// source as it was sent and on the destination as it was received.
    pub fn replica_pass(&self, volume: &str, pass: &str) -> HelperResult<PathBuf> {
        identifier("volume", volume)?;
        identifier("pass", pass)?;
        let path = self.replica_dir(volume).join(pass);
        if !held_at(&path)? {
            return Err(HelperError::NotFound(format!("pass {pass} of volume {volume}")));
        }
        Ok(path)
    }

    /// That this class can keep replicas, or why not.
    fn replicates_here(&self) -> HelperResult<()> {
        if self.backend() == Backend::Btrfs {
            return Ok(());
        }
        let why = format!(
            "storage class {:?} ({:?}) cannot send increments",
            self.name(),
            self.backend()
        );
        Err(HelperError::Refused(why))
    }

    /// The directory holding what `volume`'s replication keeps here.
    pub(crate) fn replica_dir(&self, volume: &str) -> PathBuf {
        self.root().join(REPLICAS).join(volume)
    }

    /// `volume`'s replica directory, made if it was not, and open.
    fn replica_dir_made(&self, volume: &str) -> HelperResult<File> {
        let dir = self.replica_dir(volume);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        Ok(File::open(&dir)?)
    }

    /// Takes snapshot `snapshot` of `volume` for sending, read-only, unless it was taken, and
    /// starts sending it into a pipe: whole, or what differs from `parent`, a snapshot of it sent
    /// before. With `own`, the snapshot and its parent are the volume's own snapshots, sent as
    /// they are. The pipe's read end, at whose end the stream is whole or sending failed.
    pub fn send_replica(
        &mut self,
        volume: &str,
        (snapshot, parent): (&str, &str),
        own: bool,
    ) -> HelperResult<OwnedFd> {
        identifier("volume", volume)?;
        identifier("snapshot", snapshot)?;
        self.replicates_here()?;
        let source = self.path_of(volume);
        if !is_subvolume(&source)? {
            return Err(HelperError::NotFound(format!("volume {volume}")));
        }
        let kept = if own { self.snapshot_dir(volume) } else { self.replica_dir(volume) };
        let taken = kept.join(snapshot);
        if own && !held_at(&taken)? {
            return Err(HelperError::NotFound(format!("snapshot {snapshot} of volume {volume}")));
        }
        if !own && !is_subvolume(&taken)? {
            let dir = self.replica_dir_made(volume)?;
            btrfs::snapshot(&File::open(&source)?, &dir, snapshot, true)?;
            dir.sync_all()?;
        }
        let from = if parent.is_empty() {
            None
        } else {
            identifier("parent", parent)?;
            let path = kept.join(parent);
            if !held_at(&path)? {
                return Err(HelperError::NotFound(format!("snapshot {parent} of volume {volume}")));
            }
            Some(File::open(path)?)
        };
        let snap = File::open(&taken)?;
        let (read, write) = std::io::pipe()?;
        let (v, s) = (volume.to_owned(), snapshot.to_owned());
        std::thread::Builder::new().name("replica-send".into()).spawn(move || {
            match btrfs::send(&snap, from.as_ref(), &write) {
                Ok(()) => tracing::info!(volume = %v, snapshot = %s, "replica sent"),
                Err(e) => tracing::warn!(volume = %v, snapshot = %s, error = %e, "send failed"),
            }
            drop(write);
        })?;
        Ok(OwnedFd::from(read))
    }

    /// Starts receiving snapshot `snapshot` of `volume`'s replica from a pipe: its write end,
    /// and the receiver; `None` when the snapshot is already held here whole. What a receive
    /// that ended short left of it is removed first.
    pub fn receive_replica(
        &mut self,
        volume: &str,
        snapshot: &str,
    ) -> HelperResult<Option<(OwnedFd, Child)>> {
        identifier("volume", volume)?;
        identifier("snapshot", snapshot)?;
        self.replicates_here()?;
        let dir = self.replica_dir_made(volume)?;
        let into = self.replica_dir(volume).join(snapshot);
        if held_at(&into)? {
            return Ok(None);
        }
        if is_subvolume(&into)? {
            btrfs::destroy_subvolume(&dir, snapshot)?;
        }
        let (read, write) = std::io::pipe()?;
        // Its errors reach the helper's own log; nothing is read from it, so it never blocks.
        let child = Command::new(BTRFS)
            .args(["receive", "-e", "-C"])
            .arg(self.replica_dir(volume))
            .stdin(Stdio::from(read))
            .stdout(Stdio::null())
            .spawn()?;
        Ok(Some((OwnedFd::from(write), child)))
    }

    /// What `child`, receiving snapshot `snapshot` of `volume`'s replica, came to; a failed
    /// one's partial snapshot removed.
    pub fn received(
        &mut self,
        volume: &str,
        snapshot: &str,
        child: &mut Child,
    ) -> HelperResult<Received> {
        let Some(status) = child.try_wait()? else { return Ok(Received::Running) };
        let into = self.replica_dir(volume).join(snapshot);
        if status.success() && held_at(&into)? {
            tracing::info!(%volume, %snapshot, "replica received");
            return Ok(Received::Held);
        }
        if is_subvolume(&into)? {
            btrfs::destroy_subvolume(&File::open(self.replica_dir(volume))?, snapshot)?;
        }
        let why = format!("btrfs receive ended with {status}; its errors are in the helper's log");
        tracing::warn!(%volume, %snapshot, %why, "replica not received");
        Ok(Received::Failed(why))
    }

    /// Whether snapshot `snapshot` of `volume`'s replication is held here whole.
    pub fn holds_replica(&self, volume: &str, snapshot: &str) -> HelperResult<bool> {
        identifier("volume", volume)?;
        identifier("snapshot", snapshot)?;
        held_at(&self.replica_dir(volume).join(snapshot))
    }

    /// The snapshots of `volume`'s replication held here whole, by name, in order.
    pub fn replica_snapshots(&self, volume: &str) -> HelperResult<Vec<String>> {
        identifier("volume", volume)?;
        let entries = match std::fs::read_dir(self.replica_dir(volume)) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut held = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if identifier("snapshot", &name).is_ok() && held_at(&entry.path())? {
                held.push(name);
            }
        }
        held.sort();
        Ok(held)
    }

    /// Each volume whose replication keeps a snapshot here, with those snapshots, by volume.
    pub fn replica_volumes(&self) -> HelperResult<Vec<(String, Vec<String>)>> {
        let entries = match std::fs::read_dir(self.root().join(REPLICAS)) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if identifier("volume", &name).is_err() {
                continue;
            }
            let snapshots = self.replica_snapshots(&name)?;
            if !snapshots.is_empty() {
                out.push((name, snapshots));
            }
        }
        out.sort();
        Ok(out)
    }

    /// Deletes snapshot `snapshot` of `volume`'s replication; whether it was here.
    pub fn release_replica(&mut self, volume: &str, snapshot: &str) -> HelperResult<bool> {
        identifier("volume", volume)?;
        identifier("snapshot", snapshot)?;
        if !is_subvolume(&self.replica_dir(volume).join(snapshot))? {
            return Ok(false);
        }
        btrfs::destroy_subvolume(&File::open(self.replica_dir(volume))?, snapshot)?;
        tracing::info!(%volume, %snapshot, "replica snapshot released");
        Ok(true)
    }

    /// Makes the volume `into` here a writable copy of the snapshot `snapshot` of `volume`'s
    /// replica, held whole; the copy already made of that snapshot, asked again, is the one
    /// answered, and any other volume of that identifier here is refused. Its path.
    pub fn promote_replica(
        &mut self,
        volume: &str,
        snapshot: &str,
        into: &str,
    ) -> HelperResult<PathBuf> {
        identifier("volume", volume)?;
        identifier("snapshot", snapshot)?;
        identifier("into", into)?;
        self.replicates_here()?;
        let from = self.replica_dir(volume).join(snapshot);
        if !held_at(&from)? {
            return Err(HelperError::NotFound(format!("replica {snapshot} of volume {volume}")));
        }
        let target = self.path_of(into);
        if target.symlink_metadata().is_ok() {
            let made_of = |(_, parent): ([u8; 16], [u8; 16])| parent;
            let (source, _) = btrfs::uuids(&File::open(&from)?)?;
            if is_subvolume(&target)? && made_of(btrfs::uuids(&File::open(&target)?)?) == source {
                return Ok(target);
            }
            return Err(HelperError::Refused(format!("volume {into} is here already")));
        }
        btrfs::snapshot(&File::open(&from)?, &self.handle, into, false)?;
        self.handle.sync_all()?;
        tracing::info!(%volume, %snapshot, %into, "replica promoted to the volume");
        Ok(target)
    }

    /// Makes each snapshot the replica volume `own_from` holds whole one of `into`'s own, by the
    /// same name, and releases the received copy; one `into` has already stays as it is.
    pub fn adopt_snapshots(&mut self, own_from: &str, into: &str) -> HelperResult<()> {
        identifier("own_from", own_from)?;
        identifier("into", into)?;
        let mut held = Vec::new();
        for name in self.replica_snapshots(own_from)? {
            let received = File::open(self.replica_dir(own_from).join(&name))?;
            held.push((btrfs::created(&received)?, name));
        }
        if held.is_empty() {
            return Ok(());
        }
        // Received oldest first, they are adopted in that order, which is the order a volume's own
        // snapshots are listed in: by when each was made here.
        held.sort();
        let dir = self.snapshot_dir(into);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let parent = File::open(&dir)?;
        for (_, name) in held {
            // A read-only subvolume cannot be renamed into another directory, which would change
            // its `..`, so it is snapshotted there read-only and the received copy released.
            if !is_subvolume(&dir.join(&name))? {
                let received = File::open(self.replica_dir(own_from).join(&name))?;
                btrfs::snapshot(&received, &parent, &name, true)?;
                parent.sync_all()?;
                tracing::info!(volume = %into, snapshot = %name, "snapshot adopted");
            }
            self.release_replica(own_from, &name)?;
        }
        Ok(())
    }
}
