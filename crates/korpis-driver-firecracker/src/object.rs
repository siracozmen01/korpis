// SPDX-License-Identifier: AGPL-3.0-or-later

//! A runtime object as the driver keeps it, in a directory of its own named by its identifier:
//! the spec it was created from, its config drive, and, once started, which process is its
//! Firecracker. Everything `Recover` returns and `Inspect` reads is here, so the driver can be
//! restarted while its guests run (§4.2 of docs/04-runtimes.md).

use buffa::Message as _;
use korpis_proto::korpis::driver::v1 as driver;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// The spec, in the protocol's encoding.
const SPEC: &str = "spec.binpb";

/// The process, once started.
const VM: &str = "vm.json";
/// Made when the guest is sent its stop, and removed as it is started again.
const TOLD: &str = "told";

/// The config drive.
pub const CONFIG: &str = "config";

/// A guest's Firecracker: its process, named so that a process id used again by another
/// process is never taken for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vm {
    /// Its process id.
    pub pid: i32,
    /// When the kernel says it started, in clock ticks since boot.
    pub start_ticks: u64,
    /// When the driver started it, in milliseconds since the epoch.
    pub started_at_ms: u64,
    /// When the driver saw it end.
    pub ended_at_ms: Option<u64>,
}

/// A runtime object.
#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    /// Its identifier.
    pub id: String,
    /// What it was created from.
    pub spec: driver::PreparedSpec,
    /// Its Firecracker, once started.
    pub vm: Option<Vm>,
}

fn invalid(why: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

/// Writes `bytes` at `path` whole or not at all.
fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::File::open(&tmp)?.sync_all()?;
    std::fs::rename(&tmp, path)
}

impl Object {
    /// Records a new object in `dir`.
    pub fn create(dir: &Path, id: &str, spec: &driver::PreparedSpec) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        replace(&dir.join(SPEC), &spec.encode_to_vec())?;
        Ok(Self { id: id.to_owned(), spec: spec.clone(), vm: None })
    }

    /// The object recorded in `dir`.
    pub fn load(dir: &Path) -> io::Result<Self> {
        let id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let spec = driver::PreparedSpec::decode_from_slice(&std::fs::read(dir.join(SPEC))?)
            .map_err(invalid)?;
        let vm = match std::fs::read(dir.join(VM)) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(invalid)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Ok(Self { id, spec, vm })
    }

    /// Records its Firecracker in `dir`.
    pub fn record(&mut self, dir: &Path, vm: Vm) -> io::Result<()> {
        replace(&dir.join(VM), &serde_json::to_vec(&vm).map_err(invalid)?)?;
        self.vm = Some(vm);
        Ok(())
    }

    /// Every object recorded beneath `objects`; one that cannot be read is said and skipped.
    #[must_use]
    pub fn all(objects: &Path) -> Vec<Self> {
        let Ok(entries) = std::fs::read_dir(objects) else { return Vec::new() };
        let mut all = Vec::new();
        for dir in entries.flatten().map(|e| e.path()) {
            match Self::load(&dir) {
                Ok(o) => all.push(o),
                Err(e) => tracing::warn!(dir = %dir.display(), error = %e, "an unreadable object"),
            }
        }
        all.sort_by(|a, b| a.id.cmp(&b.id));
        all
    }

    /// Its config drive, in `dir`.
    #[must_use]
    pub fn config(dir: &Path) -> PathBuf {
        dir.join(CONFIG)
    }

    /// Marks that the guest in `dir` has been sent its stop in this run: false if it already had
    /// been, since a guest reads a second as its kill, and a stop is asked again by an agent that
    /// restarts in the middle of one (§6 of docs/02-architecture.md).
    pub fn tell(dir: &Path) -> io::Result<bool> {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(TOLD)) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Clears the mark of [`Object::tell`], as a run starts or a stop is not delivered.
    pub fn untell(dir: &Path) -> io::Result<()> {
        match std::fs::remove_file(dir.join(TOLD)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_and_its_process_are_read_back_as_recorded() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("fc-1");
        let spec = driver::PreparedSpec { workload: "wl_1".into(), ..Default::default() };
        let mut made = Object::create(&dir, "fc-1", &spec).unwrap();
        assert_eq!(Object::load(&dir).unwrap(), made);
        let vm = Vm { pid: 42, start_ticks: 7, started_at_ms: 1, ended_at_ms: None };
        made.record(&dir, vm).unwrap();
        assert_eq!(Object::load(&dir).unwrap().vm, Some(vm));
        std::fs::create_dir(root.path().join("broken")).unwrap();
        assert_eq!(Object::all(root.path()), [made], "an unreadable one is skipped");
    }
}
