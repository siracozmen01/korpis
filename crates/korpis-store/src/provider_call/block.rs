// SPDX-License-Identifier: AGPL-3.0-or-later

//! Block storage calls in the stored queue (§2.1 of docs/06-storage.md). An attachment's
//! credential is held here only until the call's caller has read it, as a backup target's
//! scoped secret is: the redaction that strips those strips it too.

use korpis_core::extension::block::{
    Attachment, BlockAttach, BlockCreate, BlockDescription, BlockProtocol,
};
use korpis_core::extension::{CallOutcome, CallRequest};
use serde_json::{Value, json};
use uuid::Uuid;

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_owned)
}

fn uuid(v: &Value, key: &str) -> Option<Uuid> {
    v.get(key)?.as_str()?.parse().ok()
}

/// A block storage request to store, if `r` is one.
pub(crate) fn request_to(r: &CallRequest) -> Option<Value> {
    Some(match r {
        CallRequest::CreateBlockVolume(c) => json!({ "create_block_volume": {
            "volume": c.volume.to_string(), "storage_class": c.storage_class,
            "size_bytes": c.size_bytes } }),
        CallRequest::AttachBlockVolume(a) => {
            let protocols: Vec<&str> = a.protocols.iter().map(|p| p.as_str()).collect();
            json!({ "attach_block_volume": {
                "device": a.device, "node": a.node.to_string(), "epoch": a.epoch,
                "protocols": protocols } })
        }
        CallRequest::DetachBlockVolume { device, node } => json!({ "detach_block_volume": {
            "device": device, "node": node.to_string() } }),
        CallRequest::DeleteBlockVolume { device } => {
            json!({ "delete_block_volume": { "device": device } })
        }
        CallRequest::DescribeBlockVolume { device } => {
            json!({ "describe_block_volume": { "device": device } })
        }
        _ => return None,
    })
}

/// A stored block storage request, if `v` is one; `None` too where it is malformed.
pub(crate) fn request_from(v: &Value) -> Option<CallRequest> {
    if let Some(c) = v.get("create_block_volume") {
        let size_bytes = c.get("size_bytes")?.as_u64()?;
        let (volume, storage_class) = (uuid(c, "volume")?, text(c, "storage_class")?);
        return Some(CallRequest::CreateBlockVolume(BlockCreate {
            volume,
            storage_class,
            size_bytes,
        }));
    }
    if let Some(a) = v.get("attach_block_volume") {
        let protocols = a.get("protocols")?.as_array()?;
        let protocols: Option<Vec<BlockProtocol>> =
            protocols.iter().map(|p| BlockProtocol::parse(p.as_str()?)).collect();
        return Some(CallRequest::AttachBlockVolume(BlockAttach {
            device: text(a, "device")?,
            node: uuid(a, "node")?,
            epoch: a.get("epoch")?.as_u64()?,
            protocols: protocols?,
        }));
    }
    if let Some(d) = v.get("detach_block_volume") {
        let (device, node) = (text(d, "device")?, uuid(d, "node")?);
        return Some(CallRequest::DetachBlockVolume { device, node });
    }
    if let Some(d) = v.get("delete_block_volume") {
        return Some(CallRequest::DeleteBlockVolume { device: text(d, "device")? });
    }
    let d = v.get("describe_block_volume")?;
    Some(CallRequest::DescribeBlockVolume { device: text(d, "device")? })
}

/// Whether `v` is a stored block storage request.
pub(crate) fn is_request(v: &Value) -> bool {
    ["create", "attach", "detach", "delete", "describe"]
        .iter()
        .any(|verb| v.get(format!("{verb}_block_volume")).is_some())
}

/// A block storage outcome to store, if `o` is one.
pub(crate) fn outcome_to(o: &CallOutcome) -> Option<Value> {
    Some(match o {
        CallOutcome::BlockCreated { device } => json!({ "block_created": { "device": device } }),
        CallOutcome::BlockAttached(a) => json!({ "block_attached": {
            "protocol": a.protocol.as_str(), "target": a.target, "credential": a.credential } }),
        CallOutcome::BlockDetached => json!({ "block_detached": {} }),
        CallOutcome::BlockDeleted => json!({ "block_deleted": {} }),
        CallOutcome::BlockDescribed(d) => json!({ "block_described": {
            "size_bytes": d.size_bytes,
            "node": d.attached.map(|(n, _)| n.to_string()),
            "epoch": d.attached.map_or(0, |(_, e)| e) } }),
        _ => return None,
    })
}

/// A stored block storage outcome, if `v` is one. A redacted attachment reads with no
/// credential.
pub(crate) fn outcome_from(v: &Value) -> Option<CallOutcome> {
    if let Some(c) = v.get("block_created") {
        return Some(CallOutcome::BlockCreated { device: text(c, "device")? });
    }
    if let Some(a) = v.get("block_attached") {
        return Some(CallOutcome::BlockAttached(Attachment {
            protocol: BlockProtocol::parse(a.get("protocol")?.as_str()?)?,
            target: text(a, "target")?,
            credential: text(a, "credential").unwrap_or_default(),
        }));
    }
    if v.get("block_detached").is_some() {
        return Some(CallOutcome::BlockDetached);
    }
    if v.get("block_deleted").is_some() {
        return Some(CallOutcome::BlockDeleted);
    }
    let d = v.get("block_described")?;
    let attached = match d.get("node")? {
        Value::Null => None,
        n => Some((n.as_str()?.parse().ok()?, d.get("epoch")?.as_u64()?)),
    };
    let size_bytes = d.get("size_bytes")?.as_u64()?;
    Some(CallOutcome::BlockDescribed(BlockDescription { size_bytes, attached }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn every_block_call_and_answer_is_stored_and_read_back_whole() {
        let (volume, node) = (Uuid::now_v7(), Uuid::now_v7());
        let device = "img-1".to_owned();
        let create = BlockCreate { volume, storage_class: "fast".into(), size_bytes: 1 << 30 };
        let attach = BlockAttach {
            device: device.clone(),
            node,
            epoch: 7,
            protocols: vec![BlockProtocol::Iscsi, BlockProtocol::Loop],
        };
        for r in [
            CallRequest::CreateBlockVolume(create),
            CallRequest::AttachBlockVolume(attach),
            CallRequest::DetachBlockVolume { device: device.clone(), node },
            CallRequest::DeleteBlockVolume { device: device.clone() },
            CallRequest::DescribeBlockVolume { device: device.clone() },
        ] {
            let stored = request_to(&r).unwrap();
            assert!(is_request(&stored));
            assert_eq!(request_from(&stored), Some(r));
        }
        let attached = Attachment {
            protocol: BlockProtocol::Iscsi,
            target: "10.0.0.5:3260|iqn.2026-10.org.example:v|1".into(),
            credential: "user:secret".into(),
        };
        for o in [
            CallOutcome::BlockCreated { device },
            CallOutcome::BlockAttached(attached),
            CallOutcome::BlockDetached,
            CallOutcome::BlockDeleted,
            CallOutcome::BlockDescribed(BlockDescription {
                size_bytes: 1 << 30,
                attached: Some((node, 7)),
            }),
            CallOutcome::BlockDescribed(BlockDescription { size_bytes: 1, attached: None }),
        ] {
            assert_eq!(outcome_from(&outcome_to(&o).unwrap()), Some(o));
        }
    }
}
