// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a refusal says, as a client reads it (§8 of docs/10-api.md).

use buffa::Message;
use connectrpc::ConnectError;
use korpis_proto::korpis::v1 as pb;

/// The structured error detail of §8 of docs/10-api.md.
pub fn detail(e: &ConnectError) -> pb::Error {
    use base64::Engine;
    e.details
        .iter()
        .filter(|d| d.type_url.ends_with("korpis.v1.Error"))
        .filter_map(|d| d.value.as_deref())
        .filter_map(|v| {
            base64::engine::general_purpose::STANDARD_NO_PAD.decode(v.trim_end_matches('=')).ok()
        })
        .find_map(|bytes| pb::Error::decode_from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// The structured reason a client branches on (§8 of docs/10-api.md).
pub fn reason(e: &ConnectError) -> String {
    detail(e).reason
}
