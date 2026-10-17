// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's own backup (§6 of docs/18-operations.md): what the table there names that
//! is not the database, which is every key in the data directory (the grant signing key, the
//! assignment keyring, the CA nodes pin, the backup, TLS, and factor keys, the ACME account) and
//! the epoch watermark with its rate cap, written as one file to the target installation declared
//! outside Korpis, sealed with the key installation printed once. The database is PostgreSQL's
//! own to back up, by archiving its write-ahead log (§8 of docs/03-state.md); this says whether
//! it does, and does not pretend a dump would be the same thing.

use crate::data::DataDir;
use crate::vault::{open_bytes, seal_bytes};
use base64::Engine as _;
use korpis_store::server::{Archiving, EpochWatermark};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// What every backup file starts with, so a file is known for what it is before it is opened.
pub const MAGIC: &[u8] = b"korpis control-plane backup v1\n";
/// What a backup is sealed for, so nothing else sealed under the same key opens as one.
const CONTEXT: &str = "korpis.control-plane-backup.v1";
/// What a backup file's name starts with.
const PREFIX: &str = "korpis-control-plane-";

/// Why a backup failed.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// A file could not be read or written.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The database could not be read.
    #[error("store: {0}")]
    Store(#[from] korpis_store::Error),
    /// The data directory declares no target, or its key is not one.
    #[error("{0}")]
    Declared(String),
    /// A backup file is not one, or was not sealed with this key.
    #[error("{0}")]
    Unreadable(String),
}

/// What a backup holds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Bundle {
    /// When it was taken, in seconds since the Unix epoch.
    pub taken_at: u64,
    /// The epoch watermark and rate cap, as the database held them then.
    pub epoch: EpochWatermark,
    /// Every file under the data directory's `keys` and `tls`, by its path beneath the data
    /// directory, its contents in base64.
    pub files: BTreeMap<String, String>,
}

/// The files a backup holds: everything under `keys` and `tls`, never `backup`, whose key seals
/// the backup itself.
///
/// # Errors
/// A file could not be read.
pub fn collect(data: &DataDir) -> std::io::Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    for dir in ["keys", "tls"] {
        walk(data.root(), &data.root().join(dir), &mut files)?;
    }
    Ok(files)
}

fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<String, String>) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(root, &path, files)?;
        } else if let Ok(beneath) = path.strip_prefix(root) {
            let bytes = std::fs::read(&path)?;
            let name = beneath.to_string_lossy().into_owned();
            files.insert(name, base64::engine::general_purpose::STANDARD.encode(bytes));
        }
    }
    Ok(())
}

/// `bundle` sealed under `key`: [`MAGIC`], then the sealed JSON.
///
/// # Errors
/// Sealing failed.
pub fn seal(bundle: &Bundle, key: &[u8; 32]) -> Result<Vec<u8>, BackupError> {
    let json = zeroize::Zeroizing::new(serde_json::to_vec(bundle).map_err(std::io::Error::other)?);
    let sealed =
        seal_bytes(key, CONTEXT, &json).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok([MAGIC, &sealed].concat())
}

/// What [`seal`] sealed under `key`.
///
/// # Errors
/// It is not a backup, or was sealed under another key, or was changed.
pub fn open(bytes: &[u8], key: &[u8; 32]) -> Result<Bundle, BackupError> {
    let unreadable = |why: &str| BackupError::Unreadable(why.to_owned());
    let sealed =
        bytes.strip_prefix(MAGIC).ok_or_else(|| unreadable("not a control-plane backup"))?;
    let json = open_bytes(key, CONTEXT, sealed)
        .map_err(|_| unreadable("sealed with another key, or changed since it was written"))?;
    serde_json::from_slice(&json).map_err(|e| BackupError::Unreadable(e.to_string()))
}

/// The declared target and the sealing key, as installation wrote them beside the data directory.
///
/// # Errors
/// Either is missing or malformed.
pub fn declared(data: &DataDir) -> Result<(PathBuf, zeroize::Zeroizing<[u8; 32]>), BackupError> {
    let missing = |what: &str, e: std::io::Error| {
        BackupError::Declared(format!(
            "{what} in {}: {e}; installation declares it",
            data.backup().display()
        ))
    };
    let target = std::fs::read_to_string(data.backup().join("target"))
        .map_err(|e| missing("no backup target", e))?;
    let encoded = std::fs::read_to_string(data.backup().join("key"))
        .map_err(|e| missing("no backup key", e))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| BackupError::Declared("the backup key is not base64".into()))?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| BackupError::Declared("the backup key is not 32 bytes".into()))?;
    Ok((PathBuf::from(target.trim()), zeroize::Zeroizing::new(key)))
}

/// Writes `sealed` into `target` under a name of its time, so that it is there whole or not at all:
/// a file beside it, synced, renamed into place, and the directory synced.
///
/// # Errors
/// It could not be written.
pub fn write(target: &Path, sealed: &[u8], taken_at: u64) -> std::io::Result<PathBuf> {
    let at = time::OffsetDateTime::from_unix_timestamp(i64::try_from(taken_at).unwrap_or(0))
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let name = format!(
        "{PREFIX}{:04}{:02}{:02}T{:02}{:02}{:02}Z.kcpb",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    );
    let (path, beside) = (target.join(&name), target.join(format!(".{name}.new")));
    let mut f = std::fs::File::create(&beside)?;
    f.write_all(sealed)?;
    f.sync_all()?;
    std::fs::rename(&beside, &path)?;
    std::fs::File::open(target)?.sync_all()?;
    Ok(path)
}

/// Removes all but the newest `keep` backups in `target`; what it removed.
///
/// # Errors
/// The directory could not be read, or a file removed.
pub fn prune(target: &Path, keep: usize) -> std::io::Result<Vec<PathBuf>> {
    let mut kept: Vec<PathBuf> = std::fs::read_dir(target)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            name.starts_with(PREFIX) && p.extension().is_some_and(|e| e == "kcpb")
        })
        .collect();
    kept.sort();
    let gone = kept.len().saturating_sub(keep);
    let removed: Vec<PathBuf> = kept.drain(..gone).collect();
    for p in &removed {
        std::fs::remove_file(p)?;
    }
    Ok(removed)
}

/// What a server's archiving means for the database's backup, or why it is not one.
///
/// # Errors
/// The sentence that says why the database is not backed up.
pub fn archived(a: &Archiving) -> Result<String, String> {
    if a.mode == "off" {
        return Err("archive_mode is off, so the database is not backed up: configure WAL \
                    archiving and a base backup (§8 of docs/03-state.md)"
            .into());
    }
    if a.last_failed {
        return Err(format!(
            "archive_mode is {}, and the last attempt to archive failed ({} failures, {} \
             archived): the database's backup is falling behind",
            a.mode, a.failed, a.archived
        ));
    }
    Ok(format!("archive_mode is {}; {} segments archived, none failing", a.mode, a.archived))
}

/// What a backup did: where it is, what it held, and the database's own state.
#[derive(Debug)]
pub struct Done {
    /// The file written.
    pub path: PathBuf,
    /// How many files of the data directory it holds.
    pub files: usize,
    /// The watermark it holds.
    pub epoch: EpochWatermark,
    /// Older backups removed.
    pub pruned: Vec<PathBuf>,
    /// Whether the database is backed up by archiving, and what was found.
    pub archiving: Result<String, String>,
}

/// Backs up the control plane whose data directory is `data` and database `database_url`,
/// keeping the newest `keep`; the file is read back and opened before this returns.
///
/// # Errors
/// Nothing is declared, the database could not be read, or the file could not be written or read
/// back whole.
pub async fn backup(data: &DataDir, database_url: &str, keep: usize) -> Result<Done, BackupError> {
    let (target, key) = declared(data)?;
    let epoch = korpis_store::server::epoch_watermark(database_url).await?;
    let taken_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let bundle = Bundle { taken_at, epoch, files: collect(data)? };
    let path = write(&target, &seal(&bundle, &key)?, taken_at)?;
    if open(&std::fs::read(&path)?, &key)? != bundle {
        return Err(BackupError::Unreadable(format!("{} did not read back", path.display())));
    }
    let pruned = prune(&target, keep.max(1))?;
    let archiving = archived(&korpis_store::server::archiving(database_url).await?);
    Ok(Done { path, files: bundle.files.len(), epoch, pruned, archiving })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(files: BTreeMap<String, String>) -> Bundle {
        Bundle {
            taken_at: 1,
            epoch: EpochWatermark { max_issued: 7, issue_rate_per_sec: 1000 },
            files,
        }
    }

    #[test]
    fn a_backup_holds_the_keys_and_tls_and_not_its_own_key_and_opens_only_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path().join("data"));
        data.create().unwrap();
        std::fs::write(data.keys().join("token-1.key"), b"grant").unwrap();
        std::fs::write(data.tls().join("ca.pem"), b"ca").unwrap();
        std::fs::create_dir_all(data.backup()).unwrap();
        std::fs::write(data.backup().join("key"), b"never").unwrap();
        let files = collect(&data).unwrap();
        assert_eq!(files.keys().collect::<Vec<_>>(), ["keys/token-1.key", "tls/ca.pem"]);

        let b = bundle(files);
        let sealed = seal(&b, &[1; 32]).unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert_eq!(open(&sealed, &[1; 32]).unwrap(), b);
        assert!(matches!(open(&sealed, &[2; 32]), Err(BackupError::Unreadable(_))));
        let mut changed = sealed.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(open(&changed, &[1; 32]).is_err(), "a changed byte is refused");
    }

    #[test]
    fn the_newest_are_kept_and_only_backups_are_counted() {
        let dir = tempfile::tempdir().unwrap();
        for at in [1_700_000_000, 1_700_086_400, 1_700_172_800] {
            write(dir.path(), b"x", at).unwrap();
        }
        std::fs::write(dir.path().join("someone-else.txt"), b"mine").unwrap();
        let removed = prune(dir.path(), 2).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(removed[0].ends_with("korpis-control-plane-20231114T221320Z.kcpb"));
        assert!(dir.path().join("someone-else.txt").exists());
    }

    #[test]
    fn the_database_is_backed_up_only_when_archiving_is_on_and_not_failing() {
        let a = |mode: &str, last_failed| Archiving {
            mode: mode.into(),
            archived: 3,
            failed: 1,
            last_failed,
        };
        assert!(archived(&a("off", false)).unwrap_err().contains("not backed up"));
        assert!(archived(&a("on", true)).unwrap_err().contains("falling behind"));
        assert!(archived(&a("on", false)).is_ok());
    }
}
