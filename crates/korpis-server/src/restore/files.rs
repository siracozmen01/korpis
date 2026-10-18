// SPDX-License-Identifier: AGPL-3.0-or-later

//! The files a control-plane backup holds, put back into a data directory (§6 of
//! docs/18-operations.md): every one under `keys` and `tls`, written where it is missing, kept
//! where it is there with the same bytes, and refused where it is there with others, since a key
//! that differs from the backup's is either newer or someone else's, and only the operator can
//! say which. Nothing outside `keys` and `tls` is written, whatever the file names.

use crate::data::DataDir;
use base64::Engine as _;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Component, Path, PathBuf};

/// What putting the files back did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Placed {
    /// Files written, which the data directory lacked.
    pub written: usize,
    /// Files the data directory held already, with the same bytes.
    pub kept: usize,
}

/// Why the files could not be put back.
#[derive(Debug, thiserror::Error)]
pub enum PlaceError {
    /// A file could not be read or written.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The backup names a file outside `keys` and `tls`, or one that is not a plain path.
    #[error("the backup names {0}, which is not a file under keys or tls")]
    Outside(String),
    /// The data directory holds a file with bytes other than the backup's.
    #[error(
        "{0} differs from the backup's: move it aside if the backup's is the one to use, or \
         restore into another data directory"
    )]
    Differs(String),
    /// A file's contents are not base64.
    #[error("the backup's {0} is not base64")]
    Encoding(String),
}

/// `name`, a path beneath the data directory, if it is a plain relative path under `keys` or
/// `tls`.
fn beneath(data: &DataDir, name: &str) -> Result<PathBuf, PlaceError> {
    let outside = || PlaceError::Outside(name.to_owned());
    let path = Path::new(name);
    let plain = path.components().all(|c| matches!(c, Component::Normal(_)));
    let first = path.components().next().and_then(|c| c.as_os_str().to_str());
    if !plain || !matches!(first, Some("keys" | "tls")) || path.components().count() < 2 {
        return Err(outside());
    }
    Ok(data.root().join(path))
}

/// Writes `bytes` to `path` as the installation wrote it: a certificate (`.pem`) readable by
/// anyone, anything else by its owner alone, in directories its owner alone enters, all owned as
/// the data directory is, so a restore run as root leaves the files the control plane's user can
/// read.
fn write(data: &DataDir, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let owner = std::fs::metadata(data.root())?;
    let own = |p: &Path| std::os::unix::fs::chown(p, Some(owner.uid()), Some(owner.gid()));
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
        own(parent)?;
    }
    let public = path.extension().is_some_and(|e| e == "pem");
    let mode = if public { 0o644 } else { 0o600 };
    let mut file =
        std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    own(path)
}

/// Puts `files` back into `data`. Every file is checked before any is written, so a refusal
/// leaves the data directory as it was.
///
/// # Errors
/// A name outside `keys` and `tls`, a file that differs, or one that could not be written.
pub fn place(data: &DataDir, files: &BTreeMap<String, String>) -> Result<Placed, PlaceError> {
    let mut missing = Vec::new();
    let mut placed = Placed::default();
    for (name, encoded) in files {
        let path = beneath(data, name)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| PlaceError::Encoding(name.clone()))?;
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() && std::fs::read(&path)? == bytes => placed.kept += 1,
            Ok(_) => return Err(PlaceError::Differs(path.display().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing.push((path, bytes)),
            Err(e) => return Err(e.into()),
        }
    }
    for (path, bytes) in missing {
        write(data, &path, &bytes)?;
        placed.written += 1;
    }
    Ok(placed)
}
