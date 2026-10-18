// SPDX-License-Identifier: AGPL-3.0-or-later

//! The command inside a workload (§9.5 of docs/08-identity.md, §7 of docs/13-surface-cli.md): its
//! calls are borne by the workload's own credential, asked of the identity socket its node serves
//! it and renewed before it expires, so the command holds exactly the grants issued to the
//! workload and no secret is written into it. The control plane is reached through the relay
//! the node serves beside the socket where it serves one, and over the network otherwise, under
//! the CA the socket names, which is the one the node pinned at enrollment.

use crate::connect::Connection;
use crate::error::CliError;
use korpis_extension::identity::SOCKET;
use korpis_extension::transport::transport;
use korpis_extension::{Credential, CredentialSource as _, IdentitySocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime};

/// How long a credential is asked for while the socket says to ask again, as it does until the
/// node holds the workload's lease.
const ASKING: Duration = Duration::from_secs(60);
/// The longest wait between two askings.
const MOST: Duration = Duration::from_secs(5);
/// How long before its expiry a credential is renewed.
const RENEW_BEFORE: Duration = Duration::from_secs(300);
/// The shortest wait before a renewal, so a credential issued nearly expired is not asked for
/// in a loop.
const LEAST: Duration = Duration::from_secs(1);

/// The identity socket the command authenticates through: `named` when one is named, and
/// otherwise the one the node mounts into every workload, when no server is named and it
/// exists.
#[must_use]
pub fn socket(named: Option<PathBuf>, server_named: bool) -> Option<PathBuf> {
    if named.is_some() {
        return named;
    }
    let mounted = PathBuf::from(SOCKET);
    (!server_named && mounted.exists()).then_some(mounted)
}

/// A connection borne by the credential of the workload whose identity socket is at `socket`,
/// renewed for as long as the process runs.
///
/// # Errors
/// The socket refused, or kept saying to ask again for a minute; or what it answered is
/// malformed.
pub async fn connection(socket: &Path) -> Result<Connection, CliError> {
    let source = IdentitySocket::at(socket);
    let first = asked(&source, ASKING).await?;
    let http = transport(&first.control_plane, &first.ca_pem, first.relay.as_deref())
        .map_err(|e| CliError::Local(format!("the control plane the socket named: {e}")))?;
    let bearer = Arc::new(RwLock::new(first.secret.clone()));
    let c = Connection::borne(http, &first.control_plane, Arc::clone(&bearer))?;
    tokio::spawn(renew(source, bearer, first.expires_at));
    Ok(c)
}

/// A credential from `source`, asked again with a growing wait while it says to, for at most
/// `patience`.
async fn asked(source: &IdentitySocket, patience: Duration) -> Result<Credential, CliError> {
    let until = Instant::now() + patience;
    let mut wait = Duration::from_millis(250);
    loop {
        match source.fetch().await {
            Ok(c) => return Ok(c),
            Err(e) if e.retryable() && Instant::now() + wait < until => {
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(MOST);
            }
            Err(e) => return Err(CliError::Local(format!("the workload's identity socket: {e}"))),
        }
    }
}

/// Replaces the credential in `bearer`, which expires at `expires`, before each expires. A
/// renewal that fails is tried again until the credential in hand has expired, after which every
/// call is refused as unauthenticated and says so.
async fn renew(source: IdentitySocket, bearer: Arc<RwLock<String>>, expires: SystemTime) {
    let mut expires = expires;
    loop {
        let left = expires.duration_since(SystemTime::now()).unwrap_or_default();
        tokio::time::sleep(left.saturating_sub(RENEW_BEFORE).max(LEAST)).await;
        let left = expires.duration_since(SystemTime::now()).unwrap_or_default();
        let Ok(next) = asked(&source, left).await else { continue };
        *bearer.write().unwrap_or_else(PoisonError::into_inner) = next.secret;
        expires = next.expires_at;
    }
}

#[cfg(test)]
mod tests;
