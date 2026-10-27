// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's device key (§2.1 of docs/06-storage.md), in its data directory beside
//! its other keys. It seals the credential a block storage provider answers for a device with
//! XChaCha20-Poly1305 bound to the device's identifier, as the factor key seals a TOTP secret to
//! its account, so a sealed credential copied to another device does not open.

use crate::data::DataDir;
use crate::keys::{KeyError, load_or_create};
use crate::vault::{open_bytes, seal_bytes};
use korpis_app::ports::{DeviceVault, SealedKey, VaultError};
use uuid::Uuid;
use zeroize::Zeroizing;

/// The device key's identifier, as sealed credentials name it.
const DEVICE_KEY: &str = "device-1";

/// The device key.
pub struct DeviceKeys {
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for DeviceKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKeys").finish_non_exhaustive()
    }
}

fn context(device: Uuid) -> String {
    format!("korpis.block_credential\0{device}")
}

impl DeviceKeys {
    /// The device key of the data directory, made if there is none.
    pub fn load_or_create(data: &DataDir) -> Result<Self, KeyError> {
        let key = load_or_create(data.keys().join(format!("{DEVICE_KEY}.key")))?;
        Ok(Self { key: Zeroizing::new(key) })
    }
}

impl DeviceVault for DeviceKeys {
    fn seal(&self, device: Uuid, credential: &str) -> Result<SealedKey, VaultError> {
        let sealed = seal_bytes(&self.key, &context(device), credential.as_bytes())?;
        Ok(SealedKey { sealed_by: DEVICE_KEY.into(), sealed })
    }

    fn open(&self, device: Uuid, sealed: &SealedKey) -> Result<Zeroizing<String>, VaultError> {
        if sealed.sealed_by != DEVICE_KEY {
            return Err(VaultError::UnknownKey(sealed.sealed_by.clone()));
        }
        let plain = open_bytes(&self.key, &context(device), &sealed.sealed)?;
        let text = std::str::from_utf8(&plain).map_err(|_| VaultError::Forged(context(device)))?;
        Ok(Zeroizing::new(text.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credential_opens_for_its_own_device_alone() {
        let keys = DeviceKeys { key: Zeroizing::new([9; 32]) };
        let (one, two) = (Uuid::now_v7(), Uuid::now_v7());
        let sealed = keys.seal(one, "user:secret").unwrap();
        assert_eq!(keys.open(one, &sealed).unwrap().as_str(), "user:secret");
        assert!(matches!(keys.open(two, &sealed), Err(VaultError::Forged(_))));
        let other = SealedKey { sealed_by: "device-0".into(), ..sealed };
        assert!(matches!(keys.open(one, &other), Err(VaultError::UnknownKey(_))));
    }
}
