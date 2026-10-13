// SPDX-License-Identifier: AGPL-3.0-or-later

//! Writing so that a crash or a power cut leaves either the old file or the new one, never half
//! of either: the data is synced before the rename, and the directory after it, since a rename
//! that only reached the page cache is lost with it.

use std::io::Write as _;
use std::path::Path;

/// Syncs `dir`, so the entries created, renamed, or removed in it survive a power cut.
pub fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// The directory `path` is in.
fn parent(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Replaces `path` with `bytes` atomically and durably, readable by the owner alone.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    sync_dir(parent(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replacement_leaves_the_new_contents_and_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kept");
        replace(&path, b"old").unwrap();
        replace(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!path.with_extension("tmp").exists());
    }
}
