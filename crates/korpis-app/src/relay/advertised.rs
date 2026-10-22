// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where SFTP is served (§3.1 of docs/02-architecture.md, §3 of docs/18-operations.md). A relay
//! reports, in the feed request its identity key signs, the address clients reach its SFTP at
//! and the host key it serves, or nothing where it serves none; the control plane records what
//! changed, as an effect, since a host key replaced is what a client is warned of, and lists the
//! endpoints of every relay admitted, which `ServerInfo` names to clients.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, EffectWrite as _, RecipeSource, RelaySftp, RelayWrite as _, Store, Tx as _,
};
use korpis_core::account::ssh_key::{SshKeyKind, SshPublicKey};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::relay::Relay;
use korpis_core::relay::sftp::SftpAddress;
use uuid::Uuid;

/// An SFTP server clients are told of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpEndpoint {
    /// The relay serving it; none for the control plane's own relay role.
    pub relay: Option<Uuid>,
    /// Where clients reach it.
    pub address: SftpAddress,
    /// The host key it serves, Ed25519.
    pub host_key: SshPublicKey,
}

/// What a relay reports serving: the address and the host key, both or neither.
///
/// # Errors
/// One is given without the other, the address is not `host:port`, or the key is not Ed25519.
pub fn sftp_report(
    (host_key, address): (&[u8], &str),
) -> Result<Option<(SftpAddress, SshPublicKey)>, AppError> {
    if host_key.is_empty() && address.is_empty() {
        return Ok(None);
    }
    let parsed = SftpAddress::parse(address)
        .map_err(|_| AppError::invalid("sftp_address", "where clients reach it, host:port"))?;
    let key = SshPublicKey::from_blob(host_key.to_vec())
        .ok()
        .filter(|k| k.kind() == SshKeyKind::Ed25519)
        .ok_or_else(|| AppError::invalid("sftp_host_key", "an Ed25519 key in SSH's wire form"))?;
    Ok(Some((parsed, key)))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Records that `relay` serves SFTP as `report` says, or none, if that is not what it
    /// reported last.
    pub(super) async fn record_sftp(
        &self,
        relay: &Relay,
        report: Option<&(SftpAddress, SshPublicKey)>,
    ) -> Result<(), AppError> {
        let held = self.store.relay_sftp().await?.into_iter().find(|s| s.relay == relay.id);
        let unchanged = match (&held, report) {
            (None, None) => true,
            (Some(h), Some((a, k))) => h.address == a.to_string() && h.host_key == k.blob(),
            _ => false,
        };
        if unchanged {
            return Ok(());
        }
        let actor = Principal::System(SystemActor::Authority);
        let (id, name) = (relay.id.to_string(), relay.name.as_str());
        let mut tx = self.store.begin().await?;
        let effect = if let Some((address, key)) = report {
            let sftp = RelaySftp {
                relay: relay.id,
                address: address.to_string(),
                host_key: key.blob().to_vec(),
                reported_at: self.clock.now(),
            };
            tx.set_relay_sftp(&sftp).await?;
            Effect::new(actor, "relay.sftp_reported", Outcome::Succeeded).after(details([
                ("relay", id.into()),
                ("name", name.into()),
                ("address", sftp.address.into()),
                ("fingerprint", key.fingerprint().into()),
            ]))
        } else {
            tx.clear_relay_sftp(relay.id).await?;
            Effect::new(actor, "relay.sftp_withdrawn", Outcome::Succeeded)
                .after(details([("relay", id.into()), ("name", name.into())]))
        };
        tx.append_effect(&effect).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The SFTP endpoint of every relay admitted that reported one, by relay.
    ///
    /// # Errors
    /// The store failed, or holds an endpoint that is not one.
    pub async fn sftp_endpoints(&self) -> Result<Vec<SftpEndpoint>, AppError> {
        let admitted: Vec<Uuid> =
            self.store.relays().await?.into_iter().filter(Relay::admitted).map(|r| r.id).collect();
        let mut endpoints = Vec::new();
        for s in self.store.relay_sftp().await? {
            if !admitted.contains(&s.relay) {
                continue;
            }
            let Some((address, host_key)) = sftp_report((&s.host_key, &s.address))? else {
                continue;
            };
            endpoints.push(SftpEndpoint { relay: Some(s.relay), address, host_key });
        }
        Ok(endpoints)
    }
}
