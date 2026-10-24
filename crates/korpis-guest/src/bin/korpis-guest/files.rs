// SPDX-License-Identifier: AGPL-3.0-or-later

//! The workload's files in `/run/korpis` inside its guest (§6 of docs/04-runtimes.md): its
//! managed files in `config/` and its dependencies' files in `deps/`, copies of those its node
//! keeps in its directory on the host, which the driver sends as they change. A file is written
//! under a hidden name as its parts arrive and renamed into place, read-only, once the last has,
//! so the workload reads a whole file or the one before it, as a container does; a sync ends with
//! what each directory keeps. Nothing is written before the first process has entered the
//! workload's root, and the entry waits for the first sync, so it starts with its files as a
//! container does.

use korpis_guest::control::{Part, Place};
use std::io::{self, Seek as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Which directories the first sync has said what they keep, as bits of [`Place`].
static SYNCED: Mutex<u8> = Mutex::new(0);
/// Woken as each directory's first sync ends.
static WOKEN: Condvar = Condvar::new();
/// Both directories' bits.
const BOTH: u8 = 0b11;

fn bit(place: Place) -> u8 {
    match place {
        Place::Config => 1,
        Place::Deps => 2,
    }
}

fn dir(place: Place) -> PathBuf {
    Path::new(crate::relay::DIR).join(place.dir())
}

/// Writes the part `payload` holds; whether it was kept.
pub fn put(payload: &[u8]) -> bool {
    let Some(part) = Part::decode(payload).filter(|_| crate::relay::served()) else {
        return false;
    };
    stored(&part).is_ok()
}

fn stored(part: &Part<'_>) -> io::Result<()> {
    let dir = dir(part.place);
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
    let partial = dir.join(format!(".{}.part", part.name));
    let mut file = if part.offset == 0 {
        std::fs::File::create(&partial)?
    } else {
        let mut f = std::fs::OpenOptions::new().write(true).open(&partial)?;
        if f.metadata()?.len() != part.offset {
            return Err(io::Error::other("a part out of turn"));
        }
        f.seek(io::SeekFrom::End(0))?;
        f
    };
    file.write_all(part.data)?;
    if part.last {
        file.set_permissions(std::fs::Permissions::from_mode(0o444))?;
        std::fs::rename(&partial, dir.join(part.name))?;
    }
    Ok(())
}

/// Keeps in its directory only the files `payload` names, removing the directory if it names
/// none; whether it was done.
pub fn keep(payload: &[u8]) -> bool {
    let Some((place, names)) = korpis_guest::control::kept(payload) else { return false };
    if !crate::relay::served() || pruned(&dir(place), &names).is_err() {
        return false;
    }
    let mut synced = SYNCED.lock().unwrap_or_else(PoisonError::into_inner);
    *synced |= bit(place);
    WOKEN.notify_all();
    true
}

fn pruned(dir: &Path, names: &[&str]) -> io::Result<()> {
    if names.is_empty() {
        return match std::fs::remove_dir_all(dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if !names.iter().any(|n| std::ffi::OsStr::new(n) == name) {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Whether both directories have been synced once, waited for until `within` has passed.
pub fn synced(within: Duration) -> bool {
    let until = Instant::now() + within;
    let mut synced = SYNCED.lock().unwrap_or_else(PoisonError::into_inner);
    while *synced != BOTH {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        synced = WOKEN.wait_timeout(synced, left).unwrap_or_else(PoisonError::into_inner).0;
    }
    true
}
