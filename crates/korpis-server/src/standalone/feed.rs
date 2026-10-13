// SPDX-License-Identifier: AGPL-3.0-or-later

//! The relay's signed requests (§3.1 of docs/02-architecture.md): its feed and its certificate's
//! renewal, each signed by its identity key, fresh now and with a nonce never sent before.

use korpis_core::node::identity::{relay_feed_message, relay_renew_message, sign};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::relay::v1 as relay;
use std::time::{SystemTime, UNIX_EPOCH};

/// A feed request from `relay`, signed now with `key`.
pub fn signed_request(
    relay: &str,
    key: &ed25519_dalek::SigningKey,
) -> Result<relay::FeedRequest, getrandom::Error> {
    let auth = signed(key, |s, ns, nonce| relay_feed_message(relay, s, ns, nonce))?;
    Ok(relay::FeedRequest { relay: relay.to_owned(), auth: auth.into(), ..Default::default() })
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
