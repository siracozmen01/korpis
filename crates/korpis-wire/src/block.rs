// SPDX-License-Identifier: AGPL-3.0-or-later

//! Block storage calls on the wire (§2.1 of docs/06-storage.md, §5 of docs/16-extensions.md).

use crate::{IdKind, wire_id};
use korpis_core::extension::block::{Attachment, BlockDescription, BlockProtocol};
use korpis_core::extension::{CallOutcome, CallRequest};
use korpis_core::id::Id;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::call_result::Outcome;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;

/// The wire form of a protocol.
#[must_use]
pub const fn protocol_to_pb(p: BlockProtocol) -> pb::BlockProtocol {
    match p {
        BlockProtocol::Iscsi => pb::BlockProtocol::BLOCK_PROTOCOL_ISCSI,
        BlockProtocol::Rbd => pb::BlockProtocol::BLOCK_PROTOCOL_RBD,
        BlockProtocol::Loop => pb::BlockProtocol::BLOCK_PROTOCOL_LOOP,
    }
}

/// A protocol from the wire; none for one this control plane does not know.
#[must_use]
pub fn protocol_from_pb(p: buffa::EnumValue<pb::BlockProtocol>) -> Option<BlockProtocol> {
    match p.as_known()? {
        pb::BlockProtocol::BLOCK_PROTOCOL_ISCSI => Some(BlockProtocol::Iscsi),
        pb::BlockProtocol::BLOCK_PROTOCOL_RBD => Some(BlockProtocol::Rbd),
        pb::BlockProtocol::BLOCK_PROTOCOL_LOOP => Some(BlockProtocol::Loop),
        pb::BlockProtocol::BLOCK_PROTOCOL_UNSPECIFIED => None,
    }
}

/// A block storage request as it goes down a provider's stream, if `r` is one.
#[must_use]
pub fn request_to_pb(r: &CallRequest) -> Option<Request> {
    Some(match r {
        CallRequest::CreateBlockVolume(c) => {
            Request::CreateBlockVolume(Box::new(pb::CreateBlockVolume {
                volume: wire_id(IdKind::Volume, c.volume),
                storage_class: c.storage_class.clone(),
                size_bytes: c.size_bytes,
                ..Default::default()
            }))
        }
        CallRequest::AttachBlockVolume(a) => {
            Request::AttachBlockVolume(Box::new(pb::AttachBlockVolume {
                device: a.device.clone(),
                node: wire_id(IdKind::Node, a.node),
                epoch: a.epoch,
                protocols: a.protocols.iter().map(|p| protocol_to_pb(*p).into()).collect(),
                ..Default::default()
            }))
        }
        CallRequest::DetachBlockVolume { device, node } => {
            Request::DetachBlockVolume(Box::new(pb::DetachBlockVolume {
                device: device.clone(),
                node: wire_id(IdKind::Node, *node),
                ..Default::default()
            }))
        }
        CallRequest::DeleteBlockVolume { device } => {
            Request::DeleteBlockVolume(Box::new(pb::DeleteBlockVolume {
                device: device.clone(),
                ..Default::default()
            }))
        }
        CallRequest::DescribeBlockVolume { device } => {
            Request::DescribeBlockVolume(Box::new(pb::DescribeBlockVolume {
                device: device.clone(),
                ..Default::default()
            }))
        }
        _ => return None,
    })
}

/// A block storage answer from a provider's stream, if `o` is one it can be read as.
#[must_use]
pub fn outcome_from_pb(o: &Outcome) -> Option<CallOutcome> {
    Some(match o {
        Outcome::BlockVolumeCreated(c) => CallOutcome::BlockCreated { device: c.device.clone() },
        Outcome::BlockAttachment(a) => CallOutcome::BlockAttached(Attachment {
            protocol: protocol_from_pb(a.protocol)?,
            target: a.target.clone(),
            credential: a.credential.clone(),
        }),
        Outcome::BlockVolumeDetached(_) => CallOutcome::BlockDetached,
        Outcome::BlockVolumeDeleted(_) => CallOutcome::BlockDeleted,
        Outcome::BlockVolumeDescription(d) => {
            let attached = if d.node.is_empty() {
                None
            } else {
                Some((Id::parse(IdKind::Node, &d.node).ok()?.uuid(), d.epoch))
            };
            CallOutcome::BlockDescribed(BlockDescription { size_bytes: d.size_bytes, attached })
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_core::extension::block::BlockAttach;
    use uuid::Uuid;

    #[test]
    fn an_attach_goes_down_whole_and_its_answer_is_read_back() {
        let node = Uuid::now_v7();
        let asked = CallRequest::AttachBlockVolume(BlockAttach {
            device: "img-1".into(),
            node,
            epoch: 4,
            protocols: vec![BlockProtocol::Loop, BlockProtocol::Rbd],
        });
        let Some(Request::AttachBlockVolume(a)) = request_to_pb(&asked) else { panic!("attach") };
        assert_eq!((a.device.as_str(), a.epoch, a.protocols.len()), ("img-1", 4, 2));
        assert_eq!(a.node, wire_id(IdKind::Node, node));
        let answer = Outcome::BlockAttachment(Box::new(pb::BlockAttachment {
            protocol: pb::BlockProtocol::BLOCK_PROTOCOL_LOOP.into(),
            target: "img-1.img".into(),
            ..Default::default()
        }));
        let Some(CallOutcome::BlockAttached(got)) = outcome_from_pb(&answer) else {
            panic!("an attachment")
        };
        assert_eq!((got.protocol, got.target.as_str()), (BlockProtocol::Loop, "img-1.img"));
        let described = Outcome::BlockVolumeDescription(Box::new(pb::BlockVolumeDescription {
            size_bytes: 1 << 30,
            node: "not a node".into(),
            ..Default::default()
        }));
        assert_eq!(outcome_from_pb(&described), None, "a node that is no node");
    }
}
