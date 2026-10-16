// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's TLS key (§3.2 of docs/07-networking.md), in its data directory beside its
//! backup key and kept with it on restore. It reads the chains organizations upload, shows each
//! key to be its leaf's by comparing their public keys, and seals every certificate's key with
//! XChaCha20-Poly1305 bound to the certificate's identifier, as the backup key seals repository
//! keys, so a sealed key copied to another certificate does not open.

use crate::data::DataDir;
use crate::keys::{KeyError, load_or_create};
use crate::vault::{open_bytes, seal_bytes};
use base64::Engine as _;
use korpis_app::ports::{ReadError, SealedKey, TlsVault, VaultError};
use korpis_core::dns::certificate::Leaf;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::time::{Duration, SystemTime};
use uuid::Uuid;
use zeroize::Zeroizing;

/// The TLS key's identifier, as sealed keys name it.
const TLS_KEY: &str = "tls-1";
/// The most certificates a chain may hold.
const MAX_CHAIN: usize = 10;
/// The most bytes of PEM a chain or a key may be.
const MAX_PEM: usize = 64 * 1024;

/// The TLS key.
pub struct TlsKeys {
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for TlsKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsKeys").finish_non_exhaustive()
    }
}

fn context(certificate: Uuid) -> String {
    format!("korpis.certificate-key\0{certificate}")
}

/// `seconds` since the epoch as a time, before it as the epoch itself.
fn time(seconds: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
}

/// The certificates of `pem`, leaf first.
fn chain_of(pem: &str) -> Result<Vec<CertificateDer<'static>>, ReadError> {
    let bad = |why: &str| ReadError::Chain(why.to_owned());
    if pem.len() > MAX_PEM {
        return Err(bad("larger than 64 KiB"));
    }
    let chain = CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ReadError::Chain(format!("not PEM certificates: {e}")))?;
    match chain.len() {
        0 => Err(bad("no certificate in it")),
        n if n > MAX_CHAIN => Err(bad("more than ten certificates")),
        _ => Ok(chain),
    }
}

/// What `leaf` says: its DNS names, lowercased, and its validity.
fn leaf_of(leaf: &CertificateDer<'_>) -> Result<Leaf, ReadError> {
    use x509_parser::extensions::GeneralName;
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf)
        .map_err(|e| ReadError::Chain(format!("its first certificate does not parse: {e}")))?;
    let names = parsed
        .subject_alternative_name()
        .map_err(|e| ReadError::Chain(format!("its subject alternative names: {e}")))?
        .map(|san| {
            san.value
                .general_names
                .iter()
                .filter_map(|n| match n {
                    GeneralName::DNSName(d) => Some(d.to_ascii_lowercase()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let validity = parsed.validity();
    Ok(Leaf {
        names,
        not_before: time(validity.not_before.timestamp()),
        not_after: time(validity.not_after.timestamp()),
    })
}

/// `chain` in PEM, nothing but its certificates.
fn pem_of(chain: &[CertificateDer<'_>]) -> String {
    let mut out = String::new();
    for c in chain {
        out.push_str("-----BEGIN CERTIFICATE-----\n");
        let b64 = base64::engine::general_purpose::STANDARD.encode(c.as_ref());
        for line in b64.as_bytes().chunks(64) {
            out.push_str(&String::from_utf8_lossy(line));
            out.push('\n');
        }
        out.push_str("-----END CERTIFICATE-----\n");
    }
    out
}

/// Shows `key` to be the key of `chain`'s leaf.
fn matched(chain: &[CertificateDer<'static>], key: &str) -> Result<(), ReadError> {
    let bad = |why: String| ReadError::Key(why);
    if key.len() > MAX_PEM {
        return Err(bad("larger than 64 KiB".into()));
    }
    let der = PrivateKeyDer::from_pem_slice(key.as_bytes())
        .map_err(|e| bad(format!("not a PEM private key: {e}")))?;
    let provider = rustls::crypto::ring::default_provider();
    let signing = provider
        .key_provider
        .load_private_key(der)
        .map_err(|e| bad(format!("not a key this control plane can serve with: {e}")))?;
    rustls::sign::CertifiedKey::new(chain.to_vec(), signing)
        .keys_match()
        .map_err(|e| bad(format!("not the key of the chain's first certificate: {e}")))
}

impl TlsKeys {
    /// The TLS key of the data directory, made if there is none.
    pub fn load_or_create(data: &DataDir) -> Result<Self, KeyError> {
        let key = load_or_create(data.keys().join(format!("{TLS_KEY}.key")))?;
        Ok(Self { key: Zeroizing::new(key) })
    }
}

impl TlsVault for TlsKeys {
    fn read(&self, chain: &str, key: &str) -> Result<(Leaf, String), ReadError> {
        let certificates = chain_of(chain)?;
        let leaf = certificates.first().ok_or_else(|| ReadError::Chain("empty".into()))?;
        let read = leaf_of(leaf)?;
        matched(&certificates, key)?;
        Ok((read, pem_of(&certificates)))
    }

    fn seal(&self, certificate: Uuid, key: &str) -> Result<SealedKey, VaultError> {
        let sealed = seal_bytes(&self.key, &context(certificate), key.as_bytes())?;
        Ok(SealedKey { sealed_by: TLS_KEY.into(), sealed })
    }

    fn open(&self, certificate: Uuid, sealed: &SealedKey) -> Result<Zeroizing<String>, VaultError> {
        if sealed.sealed_by != TLS_KEY {
            return Err(VaultError::UnknownKey(sealed.sealed_by.clone()));
        }
        let plain = open_bytes(&self.key, &context(certificate), &sealed.sealed)?;
        let text =
            std::str::from_utf8(&plain).map_err(|_| VaultError::Forged(context(certificate)))?;
        Ok(Zeroizing::new(text.to_owned()))
    }
}

#[cfg(test)]
mod tests;
