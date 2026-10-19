// SPDX-License-Identifier: AGPL-3.0-or-later

//! Key management calls in the stored queue (§5.5 of docs/06-storage.md). Nothing here is a key
//! in the clear: a request names the tenant's key, the context, a one-time recipient's public key,
//! and for unwrapping the stored wrapped key; an answer is the key wrapped by the tenant's key and
//! sealed to that recipient, which only the recipient's secret, never stored, opens.

use korpis_core::extension::{CallOutcome, CallRequest};
use serde_json::{Value, json};

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        // Writing to a String cannot fail.
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    })
}

pub(super) fn unhex(v: &Value, key: &str) -> Option<Vec<u8>> {
    let s = v.get(key)?.as_str()?;
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// A key management request to store, if `r` is one.
pub(crate) fn request_to(r: &CallRequest) -> Option<Value> {
    Some(match r {
        CallRequest::GenerateKey { key, context, recipient } => json!({ "generate_key": {
            "key": key, "context": hex(context), "recipient": hex(recipient) } }),
        CallRequest::UnwrapKey { key, context, recipient, wrapped } => json!({ "unwrap_key": {
            "key": key, "context": hex(context), "recipient": hex(recipient),
            "wrapped": hex(wrapped) } }),
        _ => return None,
    })
}

/// A stored key management request, if `v` is one.
pub(crate) fn request_from(v: &Value) -> Option<CallRequest> {
    if let Some(g) = v.get("generate_key") {
        return Some(CallRequest::GenerateKey {
            key: g.get("key")?.as_str()?.to_owned(),
            context: unhex(g, "context")?,
            recipient: unhex(g, "recipient")?,
        });
    }
    let u = v.get("unwrap_key")?;
    Some(CallRequest::UnwrapKey {
        key: u.get("key")?.as_str()?.to_owned(),
        context: unhex(u, "context")?,
        recipient: unhex(u, "recipient")?,
        wrapped: unhex(u, "wrapped")?,
    })
}

/// A key management outcome to store, if `o` is one.
pub(crate) fn outcome_to(o: &CallOutcome) -> Option<Value> {
    match o {
        CallOutcome::KeyMaterial { wrapped, sealed } => Some(json!({ "key_material": {
            "wrapped": hex(wrapped), "sealed": hex(sealed) } })),
        _ => None,
    }
}

/// A stored key management outcome, if `v` is one.
pub(crate) fn outcome_from(v: &Value) -> Option<CallOutcome> {
    let k = v.get("key_material")?;
    Some(CallOutcome::KeyMaterial { wrapped: unhex(k, "wrapped")?, sealed: unhex(k, "sealed")? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_and_answers_read_back_as_they_were_stored() {
        let r = CallRequest::UnwrapKey {
            key: "projects/p/keys/k".into(),
            context: b"r/w".to_vec(),
            recipient: vec![1; 32],
            wrapped: vec![0, 255, 16],
        };
        assert_eq!(request_from(&request_to(&r).unwrap()), Some(r));
        let o = CallOutcome::KeyMaterial { wrapped: Vec::new(), sealed: vec![7; 80] };
        assert_eq!(outcome_from(&outcome_to(&o).unwrap()), Some(o));
    }
}
