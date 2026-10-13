// SPDX-License-Identifier: AGPL-3.0-or-later

//! The installation's certificate authority, issuing relays their server certificates (§3.1 of
//! docs/02-architecture.md, §7 of docs/18-operations.md). Agents and clients trust this
//! authority already, so a relay holding one of its certificates is trusted as the control plane
//! is, and nothing new has to be distributed to them.

use crate::data::DataDir;
use crate::tls::{TlsError, ca_params};
use korpis_app::ports::{CertificateIssuer, IssuedCertificate};
use rcgen::{
    CertificateParams, DnType, ExtendedKeyUsagePurpose, Issuer, KeyPair, KeyUsagePurpose,
    PublicKeyData, SignatureAlgorithm,
};
use std::time::{Duration, SystemTime};

/// The authority, loaded from the data directory.
pub struct Authority {
    key: KeyPair,
    pem: String,
}

impl std::fmt::Debug for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authority").finish_non_exhaustive()
    }
}

/// A raw P-256 public key, as a relay sends it.
struct RawKey<'a>(&'a [u8]);

impl PublicKeyData for RawKey<'_> {
    fn der_bytes(&self) -> &[u8] {
        self.0
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

/// How far before now a certificate is valid from, for clocks that run behind.
const BACKDATE: Duration = Duration::from_secs(5 * 60);

impl Authority {
    /// The authority whose certificate and key `install` wrote.
    pub fn load(data: &DataDir) -> Result<Self, TlsError> {
        let dir = data.tls();
        let pem = std::fs::read_to_string(dir.join("ca.pem"))?;
        let key = KeyPair::from_pem(&std::fs::read_to_string(dir.join("ca.key"))?)?;
        Ok(Self { key, pem })
    }

    fn sign(&self, host: &str, public_key: &[u8], lifetime: Duration) -> Result<String, TlsError> {
        let mut params = CertificateParams::new(vec![host.to_owned()])?;
        params.distinguished_name.push(DnType::CommonName, host);
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - BACKDATE;
        params.not_after = now + lifetime;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let ca = ca_params()?;
        let issuer = Issuer::from_params(&ca, &self.key);
        Ok(params.signed_by(&RawKey(public_key), &issuer)?.pem())
    }
}

impl CertificateIssuer for Authority {
    fn issue(
        &self,
        host: &str,
        public_key: &[u8],
        lifetime: Duration,
    ) -> Result<IssuedCertificate, String> {
        let pem = self.sign(host, public_key, lifetime).map_err(|e| e.to_string())?;
        Ok(IssuedCertificate {
            chain_pem: format!("{pem}{}", self.pem),
            expires_at: SystemTime::now() + lifetime,
        })
    }

    fn authority_pem(&self) -> String {
        self.pem.clone()
    }
}
