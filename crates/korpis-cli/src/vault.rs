// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where `korpis login` keeps a session's refresh credential (§7 of docs/13-surface-cli.md): the
//! operating system's keyring, one entry per control plane; where none is reachable, a file only
//! its user can read, said so when it is written; and beside either, a lock every command holds
//! while it exchanges the credential, since each exchange rotates it and two commands presenting
//! the same one at once would end the session (§9.4 of docs/08-identity.md). Beside the refresh
//! credential it keeps the last access token, until shortly before it expires, so commands run
//! one after another, a shell's completions among them, do not each rotate the credential.

use crate::error::CliError;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::sync::Arc;

/// What is kept for a session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// The refresh credential.
    pub refresh: String,
    /// The last access token it was exchanged for.
    pub access: String,
    /// When that expires, in seconds since the epoch.
    pub access_expires_at: u64,
}

/// What a keyring entry is filed under.
const SERVICE: &str = "korpis";
/// Set to `off`, the keyring is never used, and a file always is.
pub const KEYRING_VAR: &str = "KORPIS_KEYRING";

/// Where a control plane's refresh credential is kept.
pub struct Vault {
    server: String,
    dir: PathBuf,
    keyring: Option<Arc<keyring_core::CredentialStore>>,
}

/// Where a credential was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    /// The operating system's keyring.
    Keyring,
    /// A file only its user can read, because no keyring was reachable: why not.
    File(PathBuf, String),
}

fn local(what: &str, e: impl std::fmt::Display) -> CliError {
    CliError::Local(format!("{what}: {e}"))
}

impl Vault {
    /// The vault for `server`, its files under `dir`. The keyring is reached off the threads
    /// that drive the runtime, since its store blocks on its own.
    pub async fn new(server: &str, dir: PathBuf) -> Self {
        let off = std::env::var(KEYRING_VAR).is_ok_and(|v| v == "off");
        let keyring = if off {
            None
        } else {
            tokio::task::spawn_blocking(platform_store).await.ok().and_then(Result::ok)
        };
        Self { server: server.trim_end_matches('/').to_owned(), dir, keyring }
    }

    /// The directory the CLI keeps its files in: `$KORPIS_CONFIG_DIR`, or `korpis` under the
    /// user's configuration directory.
    pub fn default_dir() -> Result<PathBuf, CliError> {
        if let Some(dir) = std::env::var_os("KORPIS_CONFIG_DIR") {
            return Ok(PathBuf::from(dir));
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .ok_or_else(|| CliError::Local("no configuration directory: set HOME".into()))?;
        Ok(base.join("korpis"))
    }

    /// The stored session, if one is.
    pub async fn load(&self) -> Result<Option<Record>, CliError> {
        let text = match self.keyring_entry().await? {
            Some(found) => found,
            None => match std::fs::read_to_string(self.file()) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(local(&self.file().display().to_string(), e)),
            },
        };
        serde_json::from_str(&text).map(Some).map_err(|e| local("the kept session", e))
    }

    /// Keeps `record`, in the keyring if one is reachable and in a file otherwise.
    pub async fn save(&self, record: &Record) -> Result<Kept, CliError> {
        let secret = serde_json::to_string(record).map_err(|e| local("the session", e))?;
        let secret = secret.as_str();
        let why = match self.keyring_write(Some(secret)).await {
            Ok(()) => {
                self.remove_file()?;
                return Ok(Kept::Keyring);
            }
            Err(why) => why,
        };
        self.write_file(secret)?;
        Ok(Kept::File(self.file(), why))
    }

    /// Forgets the stored credential, wherever it is.
    pub async fn forget(&self) -> Result<(), CliError> {
        let _ = self.keyring_write(None).await;
        self.remove_file()
    }

    /// Holds the lock beside the credential until the answer is dropped, waiting for any other
    /// command that holds it.
    pub async fn lock(&self) -> Result<File, CliError> {
        self.ensure_dir()?;
        let path = self.dir.join(format!("{}.lock", self.key()));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| local(&path.display().to_string(), e))?;
        tokio::task::spawn_blocking(move || {
            rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).map(|()| file)
        })
        .await
        .map_err(|e| local("the lock", e))?
        .map_err(|e| local(&path.display().to_string(), e))
    }

    /// A file name for this control plane: the digest of its URL, so any URL makes a safe one.
    fn key(&self) -> String {
        use sha2::Digest as _;
        use std::fmt::Write as _;
        let digest = sha2::Sha256::digest(self.server.as_bytes());
        digest.iter().take(16).fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
    }

    fn file(&self) -> PathBuf {
        self.dir.join(format!("{}.session", self.key()))
    }

    fn ensure_dir(&self) -> Result<(), CliError> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| local(&self.dir.display().to_string(), e))?;
        let private = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(&self.dir, private)
            .map_err(|e| local(&self.dir.display().to_string(), e))
    }

    fn write_file(&self, secret: &str) -> Result<(), CliError> {
        self.ensure_dir()?;
        let path = self.file();
        let fail = |e: std::io::Error| local(&path.display().to_string(), e);
        let fresh = path.with_extension("session.new");
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&fresh)
            .map_err(fail)?;
        f.write_all(secret.as_bytes()).and_then(|()| f.sync_all()).map_err(fail)?;
        std::fs::rename(&fresh, &path).map_err(fail)
    }

    fn remove_file(&self) -> Result<(), CliError> {
        match std::fs::remove_file(self.file()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(local(&self.file().display().to_string(), e)),
        }
    }

    /// The keyring's entry, if there is a keyring and it holds one.
    async fn keyring_entry(&self) -> Result<Option<String>, CliError> {
        let Some(store) = self.keyring.clone() else { return Ok(None) };
        let user = self.server.clone();
        let got = tokio::task::spawn_blocking(move || {
            store.build(SERVICE, &user, None).and_then(|e| e.get_password())
        })
        .await
        .map_err(|e| local("the keyring", e))?;
        match got {
            Ok(secret) => Ok(Some(secret)),
            Err(_) => Ok(None),
        }
    }

    /// Writes `secret` to the keyring, or deletes its entry for `None`; why not, if it cannot.
    async fn keyring_write(&self, secret: Option<&str>) -> Result<(), String> {
        let Some(store) = self.keyring.clone() else {
            return Err(format!("{KEYRING_VAR}=off, or no keyring on this system"));
        };
        let (user, secret) = (self.server.clone(), secret.map(str::to_owned));
        tokio::task::spawn_blocking(move || {
            let entry = store.build(SERVICE, &user, None)?;
            match secret {
                Some(s) => entry.set_password(&s),
                None => entry.delete_credential(),
            }
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
    }
}

/// This platform's keyring: the Secret Service.
#[cfg(target_os = "linux")]
fn platform_store() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    let store: Arc<keyring_core::CredentialStore> =
        zbus_secret_service_keyring_store::Store::new()?;
    Ok(store)
}

/// This platform's keyring: the login keychain.
#[cfg(target_os = "macos")]
fn platform_store() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    let store: Arc<keyring_core::CredentialStore> =
        apple_native_keyring_store::keychain::Store::new()?;
    Ok(store)
}

/// No keyring this client knows on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_store() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    Err(keyring_core::Error::NoDefaultStore)
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault").field("server", &self.server).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn without_a_keyring_a_credential_is_kept_in_a_file_only_its_user_reads() {
        let dir = tempfile::tempdir().unwrap();
        let vault =
            Vault { server: "https://k.example".into(), dir: dir.path().join("k"), keyring: None };
        assert_eq!(vault.load().await.unwrap(), None);
        let record = Record {
            refresh: "korpisrt_x".into(),
            access: "korpisat_y".into(),
            access_expires_at: 9,
        };
        let Kept::File(path, _) = vault.save(&record).await.unwrap() else { panic!() };
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir_mode = std::fs::metadata(dir.path().join("k")).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        assert_eq!(vault.load().await.unwrap(), Some(record));
        let _held = vault.lock().await.unwrap();
        vault.forget().await.unwrap();
        assert_eq!(vault.load().await.unwrap(), None);
    }
}
