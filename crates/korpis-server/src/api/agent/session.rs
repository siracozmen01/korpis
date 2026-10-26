// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control stream (§4.2 of docs/02-architecture.md): the first agent message is a Hello,
//! the control plane answers with a Welcome declaring the session's intervals, and from then on
//! the stream carries facts up and decisions down.

use super::downlink::Downlink;
use super::published;
use super::uplink::{Uplink, journal_ack};
use crate::api::wire::{Refuse, id, timestamp};
use crate::api::{Api, ServerApp, error};
use connectrpc::{ConnectError, InboundStream};
use futures_util::StreamExt as _;
use korpis_app::node::{Hello, NodeProof, Session};
use korpis_app::ports::NodeFacts;
use korpis_core::audit::failure::Attempt;
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_wire::duration;
use korpis_wire::node::{
    advertised_from_pb, devices_from_pb, drivers_from_pb, overlay_from_pb, resources_from_pb,
    storage_classes_from_pb, unoffered_from_pb,
};
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::mpsc;

/// How many decisions may wait for a slow agent before the session is dropped and it resyncs.
const OUTBOUND: usize = 64;

/// A node's devices, those it does not offer, and its cores offered for dedication, as a Hello or
/// a heartbeat reports them.
pub(super) type Devices<'a> = (
    &'a [korpis_proto::korpis::v1::Device],
    &'a [korpis_proto::korpis::v1::UnofferedDevice],
    &'a [korpis_proto::korpis::v1::Core],
);

pub(super) fn facts(
    (capacity, allocatable): (
        Option<&korpis_proto::korpis::v1::Resources>,
        Option<&korpis_proto::korpis::v1::Resources>,
    ),
    (drivers, devices): (&[korpis_proto::korpis::v1::DriverInfo], Devices<'_>),
    classes: Option<&[korpis_proto::korpis::v1::StorageClassInfo]>,
    (advertised, overlay): (Option<&agent::Advertised>, Option<&agent::OverlayFacts>),
) -> Result<NodeFacts, ConnectError> {
    let zero = korpis_proto::korpis::v1::Resources::default();
    Ok(NodeFacts {
        capacity: resources_from_pb(capacity.unwrap_or(&zero)),
        allocatable: resources_from_pb(allocatable.unwrap_or(&zero)),
        drivers: drivers_from_pb("drivers", drivers).refuse()?,
        devices: devices_from_pb("devices", devices.0).refuse()?,
        unoffered_devices: unoffered_from_pb("unoffered_devices", devices.1).refuse()?,
        cores: korpis_wire::node::cores_from_pb("cores", devices.2).refuse()?,
        storage_classes: classes
            .map(|c| storage_classes_from_pb("storage_classes", c))
            .transpose()
            .refuse()?,
        agent: None,
        advertised: advertised
            .map(|a| advertised_from_pb("advertised", a))
            .transpose()
            .refuse()?
            .flatten(),
        overlay: overlay.map(|o| overlay_from_pb("overlay", o)).transpose().refuse()?,
    })
}

/// The proof a Hello or a `DataHello` carries for `node`, exactly as signed.
pub(in crate::api) fn proof_from_pb(
    node: &str,
    auth: Option<&agent::NodeAuth>,
) -> Result<NodeProof, ConnectError> {
    let auth = auth.ok_or_else(|| error::invalid("hello.auth", "required"))?;
    let at = auth.timestamp.as_option();
    let at = at.ok_or_else(|| error::invalid("hello.auth.timestamp", "required"))?;
    let nanos = u32::try_from(at.nanos)
        .map_err(|_| error::invalid("hello.auth.timestamp", "negative nanoseconds"))?;
    Ok(NodeProof {
        signed_node: node.to_owned(),
        seconds: at.seconds,
        nanos,
        nonce: auth.nonce.clone(),
        signature: auth.signature.clone(),
    })
}

fn hello_from_pb(h: &agent::Hello) -> Result<Hello, ConnectError> {
    Ok(Hello {
        node: id(IdKind::Node, "hello.node", &h.node).refuse()?,
        proof: proof_from_pb(&h.node, h.auth.as_option())?,
        agent_version: h.agent_version.clone(),
        protocol: h.protocol.clone(),
        facts: facts(
            (h.capacity.as_option(), h.allocatable.as_option()),
            (&h.drivers, (&h.devices, &h.unoffered_devices, &h.cores)),
            Some(&h.storage_classes),
            (h.advertised.as_option(), h.overlay.as_option()),
        )?,
    })
}

/// The Welcome; `present` asks the agent to present what it holds, while a restore settles (§8 of
/// docs/03-state.md).
fn welcome(app: &ServerApp, s: &Session, present: bool) -> agent::ControlMessage {
    agent::ControlMessage {
        message: Some(Down::Welcome(Box::new(agent::Welcome {
            protocol: s.protocol.clone(),
            heartbeat_interval: duration(s.terms.heartbeat_interval).into(),
            server_time: timestamp(app.now()).into(),
            auth_skew: duration(s.terms.auth_skew).into(),
            present_assignments: present,
            metering_interval: duration(s.terms.metering_interval).into(),
            ..Default::default()
        }))),
        ..Default::default()
    }
}

/// The token keys in force, sent before anything signed with them: a restore's rotation retired
/// the ones the agent was given at enrollment (§8 of docs/03-state.md).
async fn token_keys(app: &ServerApp) -> Result<agent::ControlMessage, ConnectError> {
    use korpis_app::ports::EnrollmentRead as _;
    let keys = app.store().verify_keys("token").await.map_err(|e| error::from_app(e.into()))?;
    let keys = agent::TokenKeys {
        keys: keys.iter().map(super::key_to_pb).collect(),
        ..Default::default()
    };
    Ok(agent::ControlMessage {
        message: Some(Down::TokenKeys(Box::new(keys))),
        ..Default::default()
    })
}

/// Reads the Hello, from `ip`, opens the session, and returns where the control plane's messages
/// go out. A Hello that does not prove its node is a failed attempt naming that node.
pub(super) async fn open(
    api: &Api,
    mut requests: InboundStream<agent::AgentMessage>,
    ip: Option<IpAddr>,
) -> Result<mpsc::Receiver<agent::ControlMessage>, ConnectError> {
    let (app, sessions) = (&api.app, &api.sessions);
    let first = requests.next().await.ok_or_else(|| error::invalid("hello", "required"))??;
    let first = first.to_owned_message();
    let Some(Up::Hello(hello)) = first.message.as_ref() else {
        return Err(error::invalid("hello", "the first message of a session is a Hello"));
    };
    let proof = hello_from_pb(hello)?;
    let named = Some(ObjectRef::new(Kind::Node, proof.node));
    let session = match app.open_session(proof).await {
        Ok(session) => session,
        Err(e) => return Err(api.failed(e, (Attempt::Hello, ip, named)).await),
    };
    let (tx, rx) = mpsc::channel(OUTBOUND);
    let present = app.settling().await.map_err(error::from_app)?.is_some();
    tx.send(welcome(app, &session, present)).await.map_err(|e| error::internal(&e))?;
    tx.send(token_keys(app).await?).await.map_err(|e| error::internal(&e))?;
    let ingested = app.journal_ingested(session.node.id).await.map_err(error::from_app)?;
    tx.send(journal_ack(ingested)).await.map_err(|e| error::internal(&e))?;
    let node = session.node.id;
    let (id, wake, ended) = sessions.register(node, tx.clone(), hello.answers_feasibility);
    tracing::info!(%node, protocol = %session.protocol, "agent session opened");
    let downlink = Downlink {
        app: Arc::clone(app),
        sessions: Arc::clone(sessions),
        node,
        id,
        tx: tx.clone(),
        wake,
        ended: ended.clone(),
        held: hello.set_digest.clone(),
    };
    let at = || (Arc::clone(sessions), node, id, ended.clone());
    let lists = api.revocations.subscribe();
    tokio::spawn(published::push(at(), tx.clone(), lists, Down::RevocationList));
    let relays = api.named_relays.subscribe();
    tokio::spawn(published::push(at(), tx.clone(), relays, Down::Relays));
    tokio::spawn(crate::mesh::push(at(), tx.clone(), api.mesh.subscribe()));
    tokio::spawn(crate::edges::push(at(), tx.clone(), api.edges.subscribe()));
    let uplink =
        Uplink { app: Arc::clone(app), sessions: Arc::clone(sessions), node, id, tx, ended };
    tokio::spawn(downlink.run());
    tokio::spawn(uplink.run(requests));
    Ok(rx)
}
