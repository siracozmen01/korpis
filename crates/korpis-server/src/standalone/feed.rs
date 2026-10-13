// SPDX-License-Identifier: AGPL-3.0-or-later

//! The relay's side of its feed (§3.1 of docs/02-architecture.md): a request signed by its
//! identity key, fresh now and with a nonce never sent before.

use korpis_core::node::identity::{relay_feed_message, sign};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::relay::v1 as relay;
use std::time::{SystemTime, UNIX_EPOCH};

/// A feed request from `relay`, signed now with `key`.
pub fn signed_request(
    relay: &str,
    key: &ed25519_dalek::SigningKey,
) -> Result<relay::FeedRequest, getrandom::Error> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)?;
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
    let nanos = since.subsec_nanos();
    let auth = agent::NodeAuth {
        timestamp: buffa_types::google::protobuf::Timestamp {
            seconds,
            nanos: i32::try_from(nanos).unwrap_or(0),
            ..Default::default()
        }
        .into(),
        nonce: nonce.to_vec(),
        signature: sign(key, &relay_feed_message(relay, seconds, nanos, &nonce)),
        ..Default::default()
    };
    Ok(relay::FeedRequest { relay: relay.to_owned(), auth: auth.into(), ..Default::default() })
}
