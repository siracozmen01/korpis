// SPDX-License-Identifier: AGPL-3.0-or-later

//! The content drive of an install in a service guest (§4.4 of docs/04-runtimes.md, §4 of
//! docs/09-recipes.md): what the install reads from the node's content store, the recipe's files
//! in `blobs/` and what its fetches downloaded in `fetched/`, each by its digest as the store
//! holds it, and the install's spec as `spec`, made an ext4 filesystem the guest mounts
//! read-only. It holds only what the install names, never the rest of the store, which holds other
//! workloads' images and files. The host writes it from its own store and parses nothing a guest
//! wrote.

use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// The file on the drive holding the install's spec.
pub const SPEC: &str = "spec";

/// What an install reads from the store, by digest (`sha256:<hex>` or `sha512:<hex>`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wanted {
    /// The recipe's files, in the store's `blobs/`.
    pub blobs: Vec<String>,
    /// What its fetches downloaded, in the store's `fetched/`.
    pub fetched: Vec<String>,
}

/// `digest` as the store names its file, `<alg>/<hex>`, if it is a digest.
fn path_of(digest: &str) -> io::Result<PathBuf> {
    let refused =
        || io::Error::new(io::ErrorKind::InvalidInput, format!("{digest:?} is no digest"));
    let (alg, hex) = digest.split_once(':').ok_or_else(refused)?;
    let long = if alg == "sha256" { 64 } else { 128 };
    let plain = hex.len() == long && hex.bytes().all(|b| b.is_ascii_hexdigit());
    if !matches!(alg, "sha256" | "sha512") || !plain {
        return Err(refused());
    }
    Ok(Path::new(alg).join(hex))
}

/// Puts the store's `from` in the tree at `to`: linked when both are on one filesystem, copied
/// otherwise. A link in the store is not followed. Its size.
fn placed(from: &Path, to: &Path) -> io::Result<u64> {
    let meta = std::fs::symlink_metadata(from)?;
    if !meta.is_file() {
        return Err(io::Error::other(format!("{} is not a file of the store", from.display())));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::hard_link(from, to).is_err() {
        std::fs::copy(from, to)?;
    }
    Ok(meta.len())
}

/// Makes `out`, the content drive of an install whose spec is `spec`, holding `wanted` from the
/// content store at `store`.
///
/// # Errors
/// A digest is malformed or not in the store, or the drive could not be made.
pub fn drive(store: &Path, wanted: &Wanted, spec: &[u8], out: &Path) -> io::Result<()> {
    let dir = out.parent().ok_or_else(|| io::Error::other("a drive needs a directory"))?;
    let work = crate::ext4::work_dir(dir, "content")?;
    let made = (|| {
        let tree = work.join("tree");
        let mut bytes = u64::try_from(spec.len()).unwrap_or(u64::MAX);
        for (place, digests) in [("blobs", &wanted.blobs), ("fetched", &wanted.fetched)] {
            std::fs::create_dir_all(tree.join(place))?;
            for digest in digests {
                let at = path_of(digest)?;
                bytes += placed(&store.join(place).join(&at), &tree.join(place).join(&at))?;
            }
        }
        std::fs::write(tree.join(SPEC), spec)?;
        std::fs::set_permissions(tree.join(SPEC), std::fs::Permissions::from_mode(0o400))?;
        // Room for the filesystem's own blocks beside the bytes: a sixteenth, and 16 MiB.
        let kib = bytes / 1024 + bytes / 1024 / 16 + 16 * 1024;
        crate::ext4::make(&tree, out, usize::try_from(kib).unwrap_or(usize::MAX), "korpis-content")
    })();
    let _ = std::fs::remove_dir_all(&work);
    made
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn a_digest_names_a_file_of_the_store_and_nothing_else() {
        let hex = "a".repeat(64);
        assert_eq!(path_of(&format!("sha256:{hex}")).unwrap(), Path::new("sha256").join(&hex));
        assert!(path_of(&format!("sha512:{}", "b".repeat(128))).is_ok());
        for bad in ["sha256:../x", "md5:abc", "sha256", &format!("sha512:{hex}")] {
            assert!(path_of(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_drive_holds_what_was_named_and_a_missing_digest_fails_it() {
        let store = tempfile::tempdir().unwrap();
        let hex = "c".repeat(64);
        std::fs::create_dir_all(store.path().join("blobs/sha256")).unwrap();
        std::fs::write(store.path().join("blobs/sha256").join(&hex), b"hello").unwrap();
        let out = tempfile::tempdir().unwrap();
        let wanted = Wanted { blobs: vec![format!("sha256:{hex}")], fetched: vec![] };
        drive(store.path(), &wanted, b"spec", &out.path().join("c.ext4")).unwrap();
        assert!(out.path().join("c.ext4").metadata().unwrap().len() > 0);
        let missing = Wanted { fetched: vec![format!("sha256:{}", "d".repeat(64))], ..wanted };
        assert!(drive(store.path(), &missing, b"spec", &out.path().join("d.ext4")).is_err());
    }
}
