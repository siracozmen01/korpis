// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading the spec directories as the node's own configuration (§6.5 of
//! docs/05-scheduling.md): a directory and a file are read only when they are owned by the
//! owner the reader was given, root on a node, and writable by no one else, a file only when it is
//! not a link and not larger than a vendor's file would be, and a hook only when its program is
//! the same.

use crate::spec::Spec;
use rustix::fs::{FileType, Mode, OFlags};
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// The largest spec file read; NVIDIA's for eight GPUs, every MIG profile of each, is far less.
const LARGEST: u64 = 4 << 20;
/// The most spec files read from one directory.
const MOST_FILES: usize = 256;

/// What one spec file held: its spec, or why it is not read.
pub type Read = (PathBuf, Result<Spec, String>);

/// Whether a file of `uid` and `mode` is `owner`'s alone to write.
fn owned(uid: u32, mode: u32, owner: u32) -> Result<(), String> {
    if uid != owner {
        return Err(format!("it is owned by uid {uid}, not {owner}"));
    }
    if mode & 0o022 != 0 {
        return Err(format!("its group or others may write it (mode {:o})", mode & 0o7777));
    }
    Ok(())
}

/// The spec files of `dir`, by name, `.json` and `.yaml`, each read or refused; nothing for a
/// directory that does not exist.
///
/// # Errors
/// The directory is not `owner`'s alone, or cannot be listed.
pub fn specs_in(dir: &Path, owner: u32) -> Result<Vec<Read>, String> {
    let meta = match std::fs::metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    if !meta.is_dir() {
        return Err("it is not a directory".to_owned());
    }
    owned(meta.uid(), meta.mode(), owner)?;
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json" || x == "yaml"))
        .collect();
    names.sort();
    if names.len() > MOST_FILES {
        return Err(format!("it holds more than {MOST_FILES} spec files"));
    }
    Ok(names.into_iter().map(|p| (p.clone(), spec_at(&p, owner))).collect())
}

/// The spec at `path`, read only when it is a file of `owner`'s alone.
fn spec_at(path: &Path, owner: u32) -> Result<Spec, String> {
    let fd =
        rustix::fs::open(path, OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
            .map_err(|e| format!("it cannot be opened, a link or otherwise: {e}"))?;
    let st = rustix::fs::fstat(&fd).map_err(|e| e.to_string())?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        return Err("it is not a regular file".to_owned());
    }
    owned(st.st_uid, st.st_mode, owner)?;
    let mut text = String::new();
    std::fs::File::from(fd)
        .take(LARGEST + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("it cannot be read as text: {e}"))?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > LARGEST {
        return Err(format!("it is larger than {LARGEST} bytes"));
    }
    if path.extension().is_some_and(|x| x == "json") {
        serde_json::from_str(&text).map_err(|e| e.to_string())
    } else {
        serde_saphyr::from_str(&text).map_err(|e| e.to_string())
    }
}

/// Whether the hook program at `path` may run as root on the host: a file of `owner`'s alone,
/// as its spec file is.
///
/// # Errors
/// Why it may not.
pub fn hook_program(path: &str, owner: u32) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("its program {path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("its program {path} is not a file"));
    }
    owned(meta.uid(), meta.mode(), owner).map_err(|e| format!("its program {path}: {e}"))
}
