// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's own backup target, declared outside Korpis (§6 of
//! docs/18-operations.md, Finding 15 of docs/23-walkthroughs.md): an installation is refused
//! without one, since a Korpis repository holding Korpis's own backup is sealed by a key whose
//! reference is in the database it would restore. What is backed up there is sealed with a key
//! installation makes, prints once, and keeps beside the data directory so a scheduled backup
//! can run; the copy that matters is the one the operator keeps elsewhere.

use crate::data::{DataDir, write_public, write_secret};
use base64::Engine as _;
use korpis_core::preflight::{Check, Outcome};
use std::path::{Path, PathBuf};

/// Where the control plane's backup goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupTarget {
    /// A directory, on another disk or a network mount.
    Directory(PathBuf),
}

impl std::str::FromStr for BackupTarget {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        if s.starts_with("s3://") {
            return Err("an S3 backup target is not built yet: name a directory on another \
                        disk or a network mount"
                .into());
        }
        let path = PathBuf::from(s);
        if !path.is_absolute() {
            return Err("the backup target is an absolute path".into());
        }
        Ok(Self::Directory(path))
    }
}

impl std::fmt::Display for BackupTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Directory(p) => write!(f, "{}", p.display()),
        }
    }
}

/// The device `path`, or the nearest of its ancestors that is there, is on.
fn device(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    path.ancestors().find_map(|p| std::fs::metadata(p).ok()).map(|m| m.dev())
}

/// Whether a file can be made in `dir`: one is made and removed.
fn writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(".korpis-preflight");
    write_public(&probe, b"").map_err(|e| e.to_string())?;
    std::fs::remove_file(&probe).map_err(|e| e.to_string())
}

/// The check of the declared target, against the data directory it is to protect.
#[must_use]
pub fn check(target: Option<&BackupTarget>, data: &Path) -> Check {
    let (name, needed_for) = ("control-plane backup target", "restoring a lost installation");
    let lacking = |found: String, costs: &str, fix: &str| Check {
        name,
        needed_for,
        found,
        outcome: Outcome::Lacking { costs: costs.into(), fix: fix.into() },
        essential: true,
    };
    let elsewhere = "--backup-target with a directory on another disk or a network mount";
    let Some(BackupTarget::Directory(dir)) = target else {
        return lacking(
            "none declared".into(),
            "the keys and the epoch watermark have nowhere outside this machine to be kept, so \
             a lost machine is a lost installation",
            elsewhere,
        );
    };
    let found = format!("{}", dir.display());
    if !dir.is_dir() {
        return lacking(
            format!("{found}: not a directory"),
            "nothing can be kept there",
            elsewhere,
        );
    }
    if let Err(e) = writable(dir) {
        return lacking(format!("{found}: {e}"), "nothing can be kept there", elsewhere);
    }
    if device(dir).is_some() && device(dir) == device(data) {
        return lacking(
            format!("{found}, on the data directory's disk"),
            "the disk the backup is on is the disk it protects",
            elsewhere,
        );
    }
    Check { name, needed_for, found, outcome: Outcome::Met, essential: true }
}

/// Records `target` in the data directory with a new sealing key; the key, to print once. A
/// data directory that already holds a declaration keeps its key, and nothing is printed.
///
/// # Errors
/// The declaration or the key could not be written.
pub fn declare(data: &DataDir, target: &BackupTarget) -> std::io::Result<Option<String>> {
    let dir = data.backup();
    std::fs::create_dir_all(&dir)?;
    let declared = dir.join("target");
    let _ = std::fs::remove_file(&declared);
    write_public(&declared, target.to_string().as_bytes())?;
    let key = dir.join("key");
    if key.exists() {
        return Ok(None);
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    write_secret(&key, encoded.as_bytes())?;
    Ok(Some(encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_a_directory_written_to_on_another_disk_and_nothing_else_will_do() {
        let none = check(None, Path::new("/var/lib/korpis-server"));
        assert!(matches!(none.outcome, Outcome::Lacking { .. }) && none.essential);
        assert!("s3://bucket/korpis".parse::<BackupTarget>().is_err());
        assert!("relative/path".parse::<BackupTarget>().is_err());
        let dir = tempfile::tempdir().unwrap();
        let target: BackupTarget = dir.path().to_str().unwrap().parse().unwrap();
        let beside = check(Some(&target), &dir.path().join("data"));
        assert_eq!(beside.found, format!("{}, on the data directory's disk", dir.path().display()));
        let missing = BackupTarget::Directory(dir.path().join("absent"));
        assert!(check(Some(&missing), Path::new("/")).found.ends_with("not a directory"));
    }

    #[test]
    fn a_declaration_makes_its_key_once() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path().join("data"));
        data.create().unwrap();
        let target = BackupTarget::Directory("/mnt/backup".into());
        let key = declare(&data, &target).unwrap().unwrap();
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&key).unwrap().len(), 32);
        assert_eq!(declare(&data, &target).unwrap(), None, "kept, and not shown again");
        let written = std::fs::read_to_string(data.backup().join("target")).unwrap();
        assert_eq!(written, "/mnt/backup");
    }
}
