// SPDX-License-Identifier: AGPL-3.0-or-later

//! The client certificates an endpoint whose TLS the proxy terminates asks for (§3.2 of
//! docs/07-networking.md): whether a client must present one, and the authorities whose
//! certificates it accepts. The authorities are public, so they travel in the declaration, as
//! DER here; reading them from PEM, and that each is a certificate, is the wire's, which reads
//! them as the proxy does.

use super::super::SpecError;

/// The most authorities one endpoint trusts.
pub const MAX_CLIENT_AUTHORITIES: usize = 16;
/// The most bytes they hold together.
pub const MAX_CLIENT_AUTHORITY_BYTES: usize = 64 << 10;

/// Whether a client must present a certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientCertificates {
    /// No handshake completes without a certificate the authorities issued.
    Required,
    /// A handshake completes without one, and the workload is told none was given; one that is
    /// given is still verified.
    Optional,
}

/// What an endpoint asks of its clients' certificates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAuth {
    /// Whether one must be presented.
    pub certificates: ClientCertificates,
    /// The certificates of the authorities trusted, in DER, 1 to [`MAX_CLIENT_AUTHORITIES`].
    pub authorities: Vec<Vec<u8>>,
}

impl ClientAuth {
    /// Checks there are 1 to [`MAX_CLIENT_AUTHORITIES`] authorities, none empty, holding at most
    /// [`MAX_CLIENT_AUTHORITY_BYTES`] together.
    ///
    /// # Errors
    /// They are none, too many, an empty one, or too large together; the error names `at`.
    pub fn check(&self, at: &str) -> Result<(), SpecError> {
        let n = self.authorities.len();
        if n == 0 || n > MAX_CLIENT_AUTHORITIES || self.authorities.iter().any(Vec::is_empty) {
            let why = format!(
                "a client certificate is verified against 1 to {MAX_CLIENT_AUTHORITIES} \
                 authorities' certificates"
            );
            return Err(SpecError::new(at, why));
        }
        if self.authorities.iter().map(Vec::len).sum::<usize>() > MAX_CLIENT_AUTHORITY_BYTES {
            let why = format!("the authorities hold at most {MAX_CLIENT_AUTHORITY_BYTES} bytes");
            return Err(SpecError::new(at, why));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn one_to_sixteen_authorities_are_trusted_within_their_bytes() {
        let auth = |authorities: Vec<Vec<u8>>| ClientAuth {
            certificates: ClientCertificates::Required,
            authorities,
        };
        assert!(auth(vec![vec![0x30; 900]]).check("tls.client_ca").is_ok());
        assert!(auth(vec![vec![0x30; 900]; 16]).check("tls.client_ca").is_ok());
        for refused in
            [Vec::new(), vec![vec![0x30; 10]; 17], vec![Vec::new()], vec![vec![0x30; 40 << 10]; 2]]
        {
            let n = refused.len();
            assert_eq!(
                auth(refused).check("tls.client_ca").unwrap_err().field,
                "tls.client_ca",
                "{n}"
            );
        }
    }
}
