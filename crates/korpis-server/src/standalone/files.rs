// SPDX-License-Identifier: AGPL-3.0-or-later

//! A relay of its own asking the control plane for its SFTP clients' file tokens (§9.9 of
//! docs/08-identity.md): `AuthorizeFiles`, signed by the relay's identity key over all it asks,
//! trusting the control plane by the authority the relay pinned when it joined.

use super::RelayDir;
use super::feed::{FilesAsked, signed_files_request};
use super::follow::control;
use crate::sftp::{Authorizer, Granted};
use connectrpc::client::HttpClient;
use korpis_proto::korpis::relay::v1 as relay;
use std::time::{Duration, SystemTime};

/// Asks for the relay kept in a directory.
pub struct AskControl {
    relay: String,
    key: ed25519_dalek::SigningKey,
    client: relay::RelayControlServiceClient<HttpClient>,
}

impl std::fmt::Debug for AskControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskControl").field("relay", &self.relay).finish_non_exhaustive()
    }
}

impl AskControl {
    /// Asks as the relay kept in `dir`.
    ///
    /// # Errors
    /// The relay has not joined, or its directory cannot be read.
    pub fn new(dir: &RelayDir) -> Result<Self, String> {
        let relay = dir.joined().map_err(|e| e.to_string())?.ok_or("this relay has not joined")?;
        let key = dir.identity().map_err(|e| e.to_string())?;
        Ok(Self { relay, key, client: control(dir)? })
    }
}

impl Authorizer for AskControl {
    async fn authorize(&self, asked: FilesAsked) -> Result<Granted, String> {
        let request = signed_files_request(&self.relay, &self.key, &asked);
        let request = request.map_err(|e| e.to_string())?;
        let done = self.client.authorize_files(request).await.map_err(|e| e.to_string())?;
        let done = done.into_owned();
        let seconds = done.expires_at.as_option().map_or(0, |t| t.seconds);
        let expires_at = SystemTime::UNIX_EPOCH
            + Duration::from_secs(u64::try_from(seconds).unwrap_or_default());
        Ok(Granted { token: done.token, expires_at, route: done.route, action: done.action })
    }
}
