// SPDX-License-Identifier: AGPL-3.0-or-later

//! The relay's signed requests (§3.1 of docs/02-architecture.md): its feed, its certificate's
//! renewal, and a file token for an SFTP client it serves (§9.9 of docs/08-identity.md), each
//! signed by its identity key, fresh now and with a nonce never sent before.

use korpis_core::node::identity::{
    files_request_digest, relay_feed_message, relay_files_message, relay_renew_message, sign,
};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::relay::v1 as relay;
use std::time::{SystemTime, UNIX_EPOCH};

/// What a relay reports serving over SFTP: its host key's wire form and the address clients
/// reach it at; none where it serves none.
pub type SftpReport = Option<(Vec<u8>, String)>;

/// A feed request from `relay` reporting `sftp`, signed now with `key`.
pub fn signed_request(
    relay: &str,
    key: &ed25519_dalek::SigningKey,
    sftp: &SftpReport,
) -> Result<relay::FeedRequest, getrandom::Error> {
    let (host_key, address) = sftp.as_ref().map_or((&[][..], ""), |(k, a)| (&k[..], &a[..]));
    let message =
        |s, ns, nonce: &[u8]| relay_feed_message((relay, (host_key, address)), s, ns, nonce);
    Ok(relay::FeedRequest {
        relay: relay.to_owned(),
        auth: signed(key, message)?.into(),
        sftp_host_key: host_key.to_vec(),
        sftp_address: address.to_owned(),
        ..Default::default()
    })
}

/// A renewal from `relay` for the new `tls_key`, signed now with `key`.
pub fn signed_renewal(
    relay: &str,
    key: &ed25519_dalek::SigningKey,
    tls_key: &[u8],
) -> Result<relay::RenewRequest, getrandom::Error> {
    let auth = signed(key, |s, ns, nonce| relay_renew_message((relay, tls_key), s, ns, nonce))?;
    Ok(relay::RenewRequest {
        relay: relay.to_owned(),
        auth: auth.into(),
        tls_key: tls_key.to_vec(),
        ..Default::default()
    })
}

/// What a relay asks a file token for: the client's SSH key in its wire form, the workload, the
/// client's address, and the token it renews, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesAsked {
    /// The key.
    pub key: Vec<u8>,
    /// The workload.
    pub workload: uuid::Uuid,
    /// The client's address.
    pub client: std::net::IpAddr,
    /// The token renewed.
    pub renews: Option<String>,
}

/// A file token's request from `relay` for `asked`, signed now with `key` over all of it.
pub fn signed_files_request(
    relay: &str,
    key: &ed25519_dalek::SigningKey,
    asked: &FilesAsked,
) -> Result<relay::AuthorizeFilesRequest, getrandom::Error> {
    let renews = asked.renews.as_deref();
    let digest = files_request_digest(&asked.key, asked.workload, asked.client, renews);
    let auth = signed(key, |s, ns, nonce| relay_files_message((relay, &digest), s, ns, nonce))?;
    Ok(relay::AuthorizeFilesRequest {
        relay: relay.to_owned(),
        auth: auth.into(),
        key: asked.key.clone(),
        workload: korpis_wire::wire_id(korpis_core::id::IdKind::Workload, asked.workload),
        client: asked.client.to_string(),
        renews: renews.unwrap_or_default().to_owned(),
        ..Default::default()
    })
}

/// `key`'s signature now, with a fresh nonce, over what `message` makes of them.
fn signed(
    key: &ed25519_dalek::SigningKey,
    message: impl FnOnce(i64, u32, &[u8]) -> Vec<u8>,
) -> Result<agent::NodeAuth, getrandom::Error> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)?;
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
    let nanos = since.subsec_nanos();
    Ok(agent::NodeAuth {
        timestamp: buffa_types::google::protobuf::Timestamp {
            seconds,
            nanos: i32::try_from(nanos).unwrap_or(0),
            ..Default::default()
        }
        .into(),
        nonce: nonce.to_vec(),
        signature: sign(key, &message(seconds, nanos, &nonce)),
        ..Default::default()
    })
}
