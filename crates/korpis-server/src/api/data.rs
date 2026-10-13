// SPDX-License-Identifier: AGPL-3.0-or-later

//! `DataService` (§3.1 of docs/02-architecture.md): a node attaches its data connection, proving
//! its key over a `DataHello`, and carries each session the relay asks it to on a stream of its
//! own. The stream a session is carried on is found by the session's identifier, which only the
//! attached node was told and which is good for one carry.

use super::agent::proof_from_pb;
use super::relay::FRAMES;
use super::wire::{Refuse as _, id};
use super::{Api, error};
use crate::relay::{Answer, Relay};
use connectrpc::{InboundStream, RequestContext, Response, ServiceResult, ServiceStream};
use futures_util::StreamExt as _;
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::attach_down::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::attach_up::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::carry_up::Message as Carried;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Response_;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Reads a node's refusals until its data connection ends, then forgets the connection.
async fn attach_uplink(
    relay: Arc<Relay>,
    (node, attachment): (Uuid, u64),
    mut requests: InboundStream<agent::AttachUp>,
) {
    while let Some(Ok(m)) = requests.next().await {
        let Some(Up::Refused(r)) = m.to_owned_message().message else { continue };
        if let Some(pending) = relay.take(&r.session) {
            // An opener that stopped waiting has nothing to be told.
            let _ = pending.answer(Answer::Refused { code: r.code, message: r.message });
        }
    }
    relay.detach(node, attachment);
    tracing::info!(%node, "data connection detached");
}

/// Forwards what the node sends to the client until either side is done.
async fn node_to_client(
    mut requests: InboundStream<agent::CarryUp>,
    to_client: mpsc::Sender<pb::RelayResponse>,
) {
    while let Some(Ok(m)) = requests.next().await {
        let Some(Carried::Frame(frame)) = m.to_owned_message().message else { continue };
        let up = pb::RelayResponse { message: Some(Response_::Frame(frame)), ..Default::default() };
        if to_client.send(up).await.is_err() {
            return;
        }
    }
}

impl agent::DataService for Api {
    async fn attach(
        &self,
        _ctx: RequestContext,
        mut requests: InboundStream<agent::AttachUp>,
    ) -> ServiceResult<ServiceStream<agent::AttachDown>> {
        let first = requests.next().await.ok_or_else(|| error::invalid("hello", "required"))??;
        let Some(Up::Hello(hello)) = first.to_owned_message().message else {
            return Err(error::invalid("hello", "the first message of an attachment is a hello"));
        };
        let node = id(IdKind::Node, "hello.node", &hello.node).refuse()?;
        let proof = proof_from_pb(&hello.node, hello.auth.as_option())?;
        self.app.attach_node(node, &proof).await.map_err(error::from_app)?;
        let (tx, mut rx) = mpsc::channel(FRAMES);
        let attached = agent::AttachDown {
            message: Some(Down::Attached(Box::default())),
            ..Default::default()
        };
        tx.send(attached).await.map_err(|e| error::internal(&e))?;
        let attachment = self.relay.attach(node, tx);
        tracing::info!(%node, "data connection attached");
        tokio::spawn(attach_uplink(Arc::clone(&self.relay), (node, attachment), requests));
        let out = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|m| m.map(Ok)));
        Response::stream_ok(out)
    }

    async fn carry(
        &self,
        _ctx: RequestContext,
        mut requests: InboundStream<agent::CarryUp>,
    ) -> ServiceResult<ServiceStream<agent::CarryDown>> {
        let first =
            requests.next().await.ok_or_else(|| error::invalid("session", "required"))??;
        let Some(Carried::Session(session)) = first.to_owned_message().message else {
            return Err(error::invalid("session", "the first message of a carry names it"));
        };
        let gone = || error::invalid("session", "not asked of this node, or carried already");
        let pending = self.relay.take(&session).ok_or_else(gone)?;
        let to_client = pending.to_client.clone();
        let opened = pb::RelayResponse {
            message: Some(Response_::Opened(Box::new(pending.opened.clone()))),
            ..Default::default()
        };
        to_client.send(opened).await.map_err(|_| gone())?;
        let (to_node, mut rx) = mpsc::channel(FRAMES);
        if !pending.answer(Answer::Carried(to_node)) {
            return Err(gone());
        }
        tokio::spawn(node_to_client(requests, to_client));
        let out = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|m| m.map(Ok)));
        Response::stream_ok(out)
    }
}
