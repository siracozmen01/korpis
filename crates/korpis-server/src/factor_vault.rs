// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's factor key (§9.6 of docs/08-identity.md), in its data directory beside its
//! backup and TLS keys. It seals every account's TOTP secret with XChaCha20-Poly1305 bound to the
//! account's identifier, as the backup key seals repository keys, so a sealed secret copied to
//! another account does not open.

use crate::data::DataDir;
use crate::keys::{KeyError, load_or_create};
use crate::vault::{open_bytes, seal_bytes};
use korpis_app::ports::{FactorVault, SealedKey, VaultError};
use korpis_core::account::totp::TotpSecret;
use uuid::Uuid;
use zeroize::Zeroizing;

/// The factor key's identifier, as sealed secrets name it.
const FACTOR_KEY: &str = "factor-1";

/// The factor key.
pub struct FactorKeys {
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for FactorKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactorKeys").finish_non_exhaustive()
    }
}

fn context(account: Uuid) -> String {
    format!("korpis.totp\0{account}")
}

impl FactorKeys {
    /// The factor key of the data directory, made if there is none.
    pub fn load_or_create(data: &DataDir) -> Result<Self, KeyError> {
        let key = load_or_create(data.keys().join(format!("{FACTOR_KEY}.key")))?;
        Ok(Self { key: Zeroizing::new(key) })
    }
}

impl FactorVault for FactorKeys {
    fn seal(&self, account: Uuid, secret: &TotpSecret) -> Result<SealedKey, VaultError> {
        let sealed = seal_bytes(&self.key, &context(account), secret.bytes())?;
        Ok(SealedKey { sealed_by: FACTOR_KEY.into(), sealed })
    }

    fn open(&self, account: Uuid, sealed: &SealedKey) -> Result<TotpSecret, VaultError> {
        if sealed.sealed_by != FACTOR_KEY {
            return Err(VaultError::UnknownKey(sealed.sealed_by.clone()));
        }
        let plain = open_bytes(&self.key, &context(account), &sealed.sealed)?;
        let bytes: [u8; 20] =
            plain.as_slice().try_into().map_err(|_| VaultError::Forged(context(account)))?;
        Ok(TotpSecret::from_bytes(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_opens_for_its_own_account_alone() {
        let keys = FactorKeys { key: Zeroizing::new([7; 32]) };
        let (one, two) = (Uuid::now_v7(), Uuid::now_v7());
        let secret = TotpSecret::generate().unwrap();
        let sealed = keys.seal(one, &secret).unwrap();
        assert_eq!(keys.open(one, &sealed).unwrap(), secret);
        assert!(matches!(keys.open(two, &sealed), Err(VaultError::Forged(_))));
        let other = SealedKey { sealed_by: "factor-0".into(), ..sealed };
        assert!(matches!(keys.open(one, &other), Err(VaultError::UnknownKey(_))));
    }
}
