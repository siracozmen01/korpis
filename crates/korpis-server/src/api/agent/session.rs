// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control stream (§4.2 of docs/02-architecture.md): the first agent message is a Hello,
//! the control plane answers with a Welcome declaring the session's intervals, and from then on
//! the stream carries facts up and decisions down.

use super::downlink::Downlink;
use super::uplink::{Uplink, journal_ack};
use crate::api::wire::{Refuse, id, timestamp};
use crate::api::{ServerApp, error};
use crate::sessions::Sessions;
use connectrpc::{ConnectError, InboundStream};
use futures_util::StreamExt as _;
use korpis_app::node::{Hello, NodeProof, Session};
use korpis_app::ports::NodeFacts;
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_wire::duration;
use korpis_wire::node::{drivers_from_pb, resources_from_pb, storage_classes_from_pb};
use std::sync::Arc;
use tokio::sync::mpsc;

/// How many decisions may wait for a slow agent before the session is dropped and it resyncs.
const OUTBOUND: usize = 64;

pub(super) fn facts(
    capacity: Option<&korpis_proto::korpis::v1::Resources>,
    allocatable: Option<&korpis_proto::korpis::v1::Resources>,
    drivers: &[korpis_proto::korpis::v1::DriverInfo],
    classes: Option<&[korpis_proto::korpis::v1::StorageClassInfo]>,
) -> Result<NodeFacts, ConnectError> {
    let zero = korpis_proto::korpis::v1::Resources::default();
    Ok(NodeFacts {
        capacity: resources_from_pb(capacity.unwrap_or(&zero)),
        allocatable: resources_from_pb(allocatable.unwrap_or(&zero)),
        drivers: drivers_from_pb("drivers", drivers).refuse()?,
        storage_classes: classes.map(storage_classes_from_pb),
        agent: None,
    })
}

fn hello_from_pb(h: &agent::Hello) -> Result<Hello, ConnectError> {
    let auth = h.auth.as_option().ok_or_else(|| error::invalid("hello.auth", "required"))?;
    let at = auth.timestamp.as_option();
    let at = at.ok_or_else(|| error::invalid("hello.auth.timestamp", "required"))?;
    let nanos = u32::try_from(at.nanos)
        .map_err(|_| error::invalid("hello.auth.timestamp", "negative nanoseconds"))?;
    Ok(Hello {
        node: id(IdKind::Node, "hello.node", &h.node).refuse()?,
        proof: NodeProof {
            signed_node: h.node.clone(),
            seconds: at.seconds,
            nanos,
            nonce: auth.nonce.clone(),
            signature: auth.signature.clone(),
        },
        agent_version: h.agent_version.clone(),
        protocol: h.protocol.clone(),
        facts: facts(
            h.capacity.as_option(),
            h.allocatable.as_option(),
            &h.drivers,
            Some(&h.storage_classes),
        )?,
    })
}

fn welcome(app: &ServerApp, s: &Session) -> agent::ControlMessage {
    agent::ControlMessage {
        message: Some(Down::Welcome(Box::new(agent::Welcome {
            protocol: s.protocol.clone(),
            heartbeat_interval: duration(s.terms.heartbeat_interval).into(),
            server_time: timestamp(app.now()).into(),
            auth_skew: duration(s.terms.auth_skew).into(),
            ..Default::default()
        }))),
        ..Default::default()
    }
}

/// Reads the Hello, opens the session, and returns where the control plane's messages go out.
pub(super) async fn open(
    app: &Arc<ServerApp>,
    sessions: &Arc<Sessions>,
    mut requests: InboundStream<agent::AgentMessage>,
) -> Result<mpsc::Receiver<agent::ControlMessage>, ConnectError> {
    let first = requests.next().await.ok_or_else(|| error::invalid("hello", "required"))??;
    let first = first.to_owned_message();
    let Some(Up::Hello(hello)) = first.message.as_ref() else {
        return Err(error::invalid("hello", "the first message of a session is a Hello"));
    };
    let session = app.open_session(hello_from_pb(hello)?).await.map_err(error::from_app)?;
    let (tx, rx) = mpsc::channel(OUTBOUND);
    tx.send(welcome(app, &session)).await.map_err(|e| error::internal(&e))?;
    let ingested = app.journal_ingested(session.node.id).await.map_err(error::from_app)?;
    tx.send(journal_ack(ingested)).await.map_err(|e| error::internal(&e))?;
    let node = session.node.id;
    let (id, wake) = sessions.register(node, tx.clone());
    tracing::info!(%node, protocol = %session.protocol, "agent session opened");
    let downlink = Downlink {
        app: Arc::clone(app),
        sessions: Arc::clone(sessions),
        node,
        id,
        tx: tx.clone(),
        wake,
        held: hello.set_digest.clone(),
    };
    let uplink = Uplink { app: Arc::clone(app), sessions: Arc::clone(sessions), node, id, tx };
    tokio::spawn(downlink.run());
    tokio::spawn(uplink.run(requests));
    Ok(rx)
}
