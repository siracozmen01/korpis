// SPDX-License-Identifier: AGPL-3.0-or-later

//! A relay's SSH host key (§3 of docs/18-operations.md): read from its file, or made there at the
//! first start where the file is absent, readable by the server alone. It is Ed25519 and nothing
//! else, so the relay never holds an RSA private key, whose signing is the operation the `rsa`
//! crate's timing advisory concerns (`deny.toml`).

pub use russh::keys::PrivateKey;
use russh::keys::ssh_key::LineEnding;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{Algorithm, HashAlg};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

/// The host key at `path`, made there first if the file is absent.
///
/// # Errors
/// The file could not be read or written, is not an OpenSSH private key, or is not Ed25519.
pub fn host_key(path: &Path) -> Result<PrivateKey, String> {
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    if !path.exists() {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| at(&e))?;
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
        let pem = key.to_openssh(LineEnding::LF).map_err(|e| at(&e))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| at(&e))?;
        file.write_all(pem.as_bytes()).and_then(|()| file.sync_all()).map_err(|e| at(&e))?;
    }
    let pem = std::fs::read_to_string(path).map_err(|e| at(&e))?;
    let key = PrivateKey::from_openssh(pem).map_err(|e| at(&e))?;
    if key.algorithm() != Algorithm::Ed25519 {
        return Err(at(&"an SFTP host key is Ed25519"));
    }
    Ok(key)
}

/// `key`'s fingerprint as `ssh-keygen -l` prints it, which a client is shown on connecting.
#[must_use]
pub fn fingerprint(key: &PrivateKey) -> String {
    key.public_key().fingerprint(HashAlg::Sha256).to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn a_host_key_is_made_once_readable_by_the_server_alone_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sftp_host_key");
        let made = host_key(&path).unwrap();
        let permissions = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(permissions, 0o600);
        let again = host_key(&path).unwrap();
        assert_eq!(fingerprint(&made), fingerprint(&again), "kept, not made again");
        assert!(fingerprint(&made).starts_with("SHA256:"));
    }

    #[test]
    fn a_host_key_of_another_kind_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not_a_key");
        std::fs::write(&path, "not a key").unwrap();
        assert!(host_key(&path).is_err());
    }
}
