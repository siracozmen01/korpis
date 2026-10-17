// SPDX-License-Identifier: AGPL-3.0-or-later

//! Why the control stream ended, with the one end that dialling again soon cannot mend told
//! apart: a control plane that refuses this agent's protocol (§4 of docs/18-operations.md), which
//! only an upgrade of one side or the other does.

use base64::Engine as _;
use buffa::Message as _;
use connectrpc::ConnectError;
use korpis_proto::korpis::v1 as pb;

/// Why the stream ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The control plane refuses this agent's protocol, and says which side to upgrade.
    Refused(String),
    /// Anything else: dialling again may mend it.
    Other(String),
}

impl From<String> for Ended {
    fn from(s: String) -> Self {
        Self::Other(s)
    }
}

impl From<&str> for Ended {
    fn from(s: &str) -> Self {
        Self::Other(s.to_owned())
    }
}

impl From<ConnectError> for Ended {
    fn from(e: ConnectError) -> Self {
        let refused = e
            .details
            .iter()
            .filter(|d| d.type_url.ends_with("korpis.v1.Error"))
            .filter_map(|d| d.value.as_deref())
            .filter_map(|v| {
                let v = v.trim_end_matches('=');
                base64::engine::general_purpose::STANDARD_NO_PAD.decode(v).ok()
            })
            .filter_map(|bytes| pb::Error::decode_from_slice(&bytes).ok())
            .any(|detail| detail.reason == "protocol.unsupported");
        if refused {
            Self::Refused(e.message.unwrap_or_default())
        } else {
            Self::Other(e.to_string())
        }
    }
}

impl std::fmt::Display for Ended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) | Self::Other(why) => f.write_str(why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use connectrpc::{ErrorCode, ErrorDetail};

    #[test]
    fn a_refusal_of_the_protocol_is_told_apart_by_its_reason() {
        let detail = pb::Error { reason: "protocol.unsupported".into(), ..Default::default() };
        let mut refusal = ConnectError::new(ErrorCode::FailedPrecondition, "upgrade the agent");
        refusal.details.push(ErrorDetail {
            type_url: "type.googleapis.com/korpis.v1.Error".into(),
            value: Some(base64::engine::general_purpose::STANDARD.encode(detail.encode_to_vec())),
            debug: None,
        });
        assert_eq!(Ended::from(refusal), Ended::Refused("upgrade the agent".into()));
        let other = ConnectError::new(ErrorCode::Unavailable, "down");
        assert!(matches!(Ended::from(other), Ended::Other(_)));
    }
}
