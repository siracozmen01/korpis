// SPDX-License-Identifier: AGPL-3.0-or-later

//! The files a control-plane backup holds, put back into a data directory (§6 of
//! docs/18-operations.md): every one under `keys` and `tls`, written where it is missing, kept
//! where it is there with the same bytes, and refused where it is there with others, since a key
//! that differs from the backup's is either newer or someone else's, and only the operator can
//! say which. Nothing outside `keys` and `tls` is written, whatever the file names. It is written
//! by the data directory's owner and no one else, as the backup is read, and through no link: what
//! a compromised control plane could put in its own data directory steers nothing a restore run
//! by a more privileged user would write.

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
    /// A link where a directory or a file is to be.
    #[error("{0} is a link; a restore writes through none")]
    Link(String),
    /// This process does not run as the data directory's owner.
    #[error(
        "the data directory belongs to uid {owner} and this runs as uid {running}: run the \
         restore as the data directory's owner, as the control plane runs"
    )]
    Owner {
        /// The data directory's owner.
        owner: u32,
        /// Who this runs as.
        running: u32,
    },
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

/// Refuses a link at `path` or any directory between it and the data directory.
fn no_link(data: &DataDir, path: &Path) -> Result<(), PlaceError> {
    let mut at = data.root().to_path_buf();
    let rest = path.strip_prefix(data.root()).unwrap_or(path);
    for part in std::iter::once(None).chain(rest.components().map(Some)) {
        if let Some(part) = part {
            at.push(part);
        }
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(PlaceError::Link(at.display().to_string()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Refuses unless this process runs as the data directory's owner.
fn as_owner(data: &DataDir) -> Result<(), PlaceError> {
    use std::os::unix::fs::MetadataExt as _;
    let owner = std::fs::symlink_metadata(data.root())?.uid();
    let running = std::fs::metadata("/proc/self")?.uid();
    if owner == running { Ok(()) } else { Err(PlaceError::Owner { owner, running }) }
}

/// Writes `bytes` to `path` as the installation wrote it: a certificate (`.pem`) readable by
/// anyone, anything else by its owner alone, in directories its owner alone enters.
fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    }
    let public = path.extension().is_some_and(|e| e == "pem");
    let mode = if public { 0o644 } else { 0o600 };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Puts `files` back into `data`. Every file is checked before any is written, so a refusal
/// leaves the data directory as it was.
///
/// # Errors
/// A name outside `keys` and `tls`, a file that differs, or one that could not be written.
pub fn place(data: &DataDir, files: &BTreeMap<String, String>) -> Result<Placed, PlaceError> {
    as_owner(data)?;
    let mut missing = Vec::new();
    let mut placed = Placed::default();
    for (name, encoded) in files {
        let path = beneath(data, name)?;
        no_link(data, &path)?;
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
        write(&path, &bytes)?;
        placed.written += 1;
    }
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(name: &str) -> BTreeMap<String, String> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"key");
        BTreeMap::from([(name.to_owned(), encoded)])
    }

    #[test]
    fn a_file_is_put_back_once_and_never_outside_keys_and_tls() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path().to_path_buf());
        assert_eq!(place(&data, &one("keys/token-1.key")).unwrap(), Placed { written: 1, kept: 0 });
        assert_eq!(place(&data, &one("keys/token-1.key")).unwrap(), Placed { written: 0, kept: 1 });
        for outside in ["backup/key", "keys/../backup/key", "/etc/passwd", "keys"] {
            assert!(
                matches!(place(&data, &one(outside)), Err(PlaceError::Outside(_))),
                "{outside}"
            );
        }
    }

    #[test]
    fn a_link_in_the_data_directory_is_written_through_by_no_restore() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path().join("data"));
        std::fs::create_dir(data.root()).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), data.root().join("keys")).unwrap();
        let refused = place(&data, &one("keys/token-1.key"));
        assert!(matches!(refused, Err(PlaceError::Link(_))), "{refused:?}");
        assert!(!elsewhere.path().join("token-1.key").exists(), "nothing written through it");
    }

    #[test]
    fn a_restore_run_by_anyone_but_the_data_directorys_owner_writes_nothing() {
        use std::os::unix::fs::MetadataExt as _;
        if std::fs::metadata("/proc/self").map_or(1, |m| m.uid()) != 0 {
            return; // Only root can give a directory away to test this.
        }
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path().join("data"));
        std::fs::create_dir(data.root()).unwrap();
        std::os::unix::fs::chown(data.root(), Some(65_534), Some(65_534)).unwrap();
        let refused = place(&data, &one("keys/token-1.key"));
        assert!(matches!(refused, Err(PlaceError::Owner { owner: 65_534, .. })), "{refused:?}");
        assert!(!data.keys().exists());
    }
}
