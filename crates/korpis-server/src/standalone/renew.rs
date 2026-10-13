// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keeping the relay's certificate current (§7 of docs/18-operations.md). With about a third of
//! its life left, shifted by up to a day fixed by the relay's identifier so relays do not all
//! ask at once, the relay makes a fresh TLS key, asks the control plane to renew, writes the new
//! generation in full, points its directory at it, and serves it from the next handshake. A
//! renewal that fails is asked again, backing off to an hour; the control plane reports the
//! approaching expiry as events meanwhile.

use super::RelayDir;
use super::feed::signed_renewal;
use super::follow::control;
use super::served::{Served, load};
use rcgen::KeyPair;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How much of its life a certificate has left when it is renewed: about a third of the
/// ninety days relays are issued.
pub const RENEW_WITHIN: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// The longest wait between checks, and between failed attempts.
const LONGEST: Duration = Duration::from_secs(60 * 60);

/// When the certificate served from `dir` is due for renewal: [`RENEW_WITHIN`] before it expires,
/// less up to a day fixed by `relay`. Now if its expiry was not recorded.
fn due(dir: &RelayDir, relay: &str) -> std::io::Result<SystemTime> {
    let files = dir.tls_files(dir.tls_generation()?);
    let Some(expires) = dir.expires(&files)? else { return Ok(UNIX_EPOCH) };
    let spread = relay.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
    let shift = Duration::from_secs(spread % (24 * 60 * 60));
    Ok(expires.checked_sub(RENEW_WITHIN + shift).unwrap_or(UNIX_EPOCH))
}

/// Renews once; when the new certificate expires.
pub async fn renew_once(
    dir: &RelayDir,
    relay: &str,
    served: &Served,
) -> Result<SystemTime, String> {
    let next = dir.tls_generation().map_err(|e| e.to_string())? + 1;
    let files = dir.tls_files(next);
    let key = KeyPair::generate().map_err(|e| e.to_string())?;
    dir.secret(&files.key, key.serialize_pem().as_bytes()).map_err(|e| e.to_string())?;
    let identity = dir.identity().map_err(|e| e.to_string())?;
    let request =
        signed_renewal(relay, &identity, key.public_key_raw()).map_err(|e| e.to_string())?;
    let renewed = control(dir)?.renew(request).await.map_err(|e| e.to_string())?.into_owned();
    let seconds = renewed.certificate_expires_at.as_option().map_or(0, |t| t.seconds);
    dir.public_at(&files.chain, renewed.certificate_chain_pem.as_bytes())
        .and_then(|()| dir.public_at(&files.expires, seconds.to_string().as_bytes()))
        .map_err(|e| e.to_string())?;
    let certified = load(&files).map_err(|e| e.to_string())?;
    dir.switch_tls(next).map_err(|e| e.to_string())?;
    served.replace(certified);
    let seconds = u64::try_from(seconds).unwrap_or(0);
    Ok(UNIX_EPOCH + Duration::from_secs(seconds))
}

/// Keeps the certificate `served` holds current, for as long as the relay runs.
pub async fn keep_renewed(dir: RelayDir, relay: String, served: Arc<Served>) {
    let mut backoff = Duration::from_secs(60);
    loop {
        let wait = match due(&dir, &relay) {
            Ok(at) => at.duration_since(SystemTime::now()).unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = %e, "the certificate's expiry is unreadable; renewing");
                Duration::ZERO
            }
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait.min(LONGEST)).await;
            continue;
        }
        match renew_once(&dir, &relay, &served).await {
            Ok(expires) => {
                let days = expires.duration_since(SystemTime::now()).unwrap_or_default().as_secs()
                    / 86_400;
                tracing::info!(days, "certificate renewed");
                backoff = Duration::from_secs(60);
            }
            Err(e) => {
                tracing::warn!(error = %e, "renewing the certificate failed; asking again in {backoff:?}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(LONGEST);
            }
        }
    }
}
