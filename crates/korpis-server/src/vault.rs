// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's key vault for `operator` custody (§5.5 of docs/06-storage.md): a backup
//! key in the data directory, which seals each repository's key-encryption key, which seals each
//! workload's data key. Each seal is XChaCha20-Poly1305 under a fresh random nonce with what it is
//! for bound in as associated data, so a sealed key copied to another repository or workload does
//! not open. The backup key is kept on restore, like the assignment key: without it no backup
//! made under it can be read (§8 of docs/03-state.md).

use crate::data::DataDir;
use crate::keys::{KeyError, load_or_create};
use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use korpis_app::ports::{DataKey, KeyRecipient, KeyVault, SealedKey, VaultError};
use uuid::Uuid;
use zeroize::Zeroizing;

/// The backup key's identifier, as sealed keys name it.
const BACKUP_KEY: &str = "backup-1";
const NONCE: usize = 24;

/// The vault.
pub struct Vault {
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault").finish_non_exhaustive()
    }
}

fn failed(e: impl std::fmt::Display) -> VaultError {
    VaultError::Failed(e.to_string())
}

fn random() -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let mut k = Zeroizing::new([0u8; 32]);
    getrandom::fill(k.as_mut()).map_err(failed)?;
    Ok(k)
}

fn seal(key: &[u8; 32], context: &str, plain: &[u8; 32]) -> Result<Vec<u8>, VaultError> {
    let mut nonce = [0u8; NONCE];
    getrandom::fill(&mut nonce).map_err(failed)?;
    let payload = Payload { msg: plain, aad: context.as_bytes() };
    let sealed = XChaCha20Poly1305::new(key.into())
        .encrypt(&XNonce::from(nonce), payload)
        .map_err(failed)?;
    Ok([nonce.as_slice(), &sealed].concat())
}

fn open(key: &[u8; 32], context: &str, sealed: &[u8]) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let forged = || VaultError::Forged(context.to_owned());
    let (nonce, ciphertext) = sealed.split_at_checked(NONCE).ok_or_else(forged)?;
    let nonce: [u8; NONCE] = nonce.try_into().map_err(|_| forged())?;
    let payload = Payload { msg: ciphertext, aad: context.as_bytes() };
    let plain = Zeroizing::new(
        XChaCha20Poly1305::new(key.into())
            .decrypt(&XNonce::from(nonce), payload)
            .map_err(|_| forged())?,
    );
    let key: [u8; 32] = plain.as_slice().try_into().map_err(|_| forged())?;
    Ok(Zeroizing::new(key))
}

fn kek_context(repository: Uuid) -> String {
    format!("korpis.repository-key\0{repository}")
}

fn dek_context(repository: Uuid, workload: Uuid) -> String {
    format!("korpis.workload-key\0{repository}\0{workload}")
}

impl Vault {
    /// The vault over the data directory's backup key, made if there is none.
    pub fn load_or_create(data: &DataDir) -> Result<Self, KeyError> {
        let key = load_or_create(data.keys().join(format!("{BACKUP_KEY}.key")))?;
        Ok(Self { key: Zeroizing::new(key) })
    }

    fn kek(&self, kek: &SealedKey, repository: Uuid) -> Result<Zeroizing<[u8; 32]>, VaultError> {
        if kek.sealed_by != BACKUP_KEY {
            return Err(VaultError::UnknownKey(kek.sealed_by.clone()));
        }
        open(&self.key, &kek_context(repository), &kek.sealed)
    }
}

impl KeyVault for Vault {
    fn new_kek(&self, repository: Uuid) -> Result<SealedKey, VaultError> {
        let kek = random()?;
        Ok(SealedKey {
            sealed_by: BACKUP_KEY.into(),
            sealed: seal(&self.key, &kek_context(repository), &kek)?,
        })
    }

    fn new_dek(
        &self,
        kek: &SealedKey,
        repository: Uuid,
        workload: Uuid,
    ) -> Result<SealedKey, VaultError> {
        let kek = self.kek(kek, repository)?;
        let dek = random()?;
        let sealed = seal(&kek, &dek_context(repository, workload), &dek)?;
        Ok(SealedKey { sealed_by: format!("repository:{repository}"), sealed })
    }

    fn open_dek(
        &self,
        kek: &SealedKey,
        dek: &SealedKey,
        repository: Uuid,
        workload: Uuid,
    ) -> Result<DataKey, VaultError> {
        let kek = self.kek(kek, repository)?;
        Ok(DataKey(open(&kek, &dek_context(repository, workload), &dek.sealed)?))
    }

    fn recipient(&self) -> Result<Box<dyn KeyRecipient>, VaultError> {
        let made = korpis_extension::keyseal::Recipient::new()
            .map_err(|e| VaultError::Failed(e.to_string()))?;
        Ok(Box::new(Once(made)))
    }
}

/// A recipient made for one opening.
struct Once(korpis_extension::keyseal::Recipient);

impl KeyRecipient for Once {
    fn public(&self) -> Vec<u8> {
        self.0.public().to_vec()
    }

    fn open(&self, sealed: &[u8], context: &[u8]) -> Result<DataKey, VaultError> {
        let key = self.0.open(sealed, context).map_err(|e| VaultError::Forged(e.to_string()))?;
        Ok(DataKey(Zeroizing::new(key)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_data_key_opens_only_for_its_repository_and_workload() {
        let v = Vault { key: Zeroizing::new([3; 32]) };
        let (repo, other, wl) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let kek = v.new_kek(repo).unwrap();
        let dek = v.new_dek(&kek, repo, wl).unwrap();
        let opened = v.open_dek(&kek, &dek, repo, wl).unwrap();
        assert_eq!(
            *opened.0,
            *v.open_dek(&kek, &dek, repo, wl).unwrap().0,
            "the same key each time"
        );
        assert!(matches!(v.open_dek(&kek, &dek, repo, Uuid::now_v7()), Err(VaultError::Forged(_))));
        assert!(
            matches!(v.open_dek(&kek, &dek, other, wl), Err(VaultError::Forged(_))),
            "the kek is bound too"
        );
        let elsewhere = Vault { key: Zeroizing::new([4; 32]) };
        assert!(elsewhere.open_dek(&kek, &dek, repo, wl).is_err());
    }
}
