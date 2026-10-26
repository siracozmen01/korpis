// SPDX-License-Identifier: AGPL-3.0-or-later

//! `AgentService` (§4 of docs/02-architecture.md): enrollment, and the control stream. The
//! agent dials out to the same listener the clients use, over the same TLS.

mod certificate;
mod credential;
mod downlink;
mod edge_lease;
mod health;
mod job;
mod published;
mod session;
mod step;
mod uplink;

pub(super) use session::proof_from_pb;

use super::Api;
use super::wire::{timestamp, wire_id};
use connectrpc::{
    InboundStream, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use korpis_app::node::{Enroll, Enrolled};
use korpis_app::ports::VerifyKey;
use korpis_core::audit::failure::Attempt;
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;

pub(super) fn key_to_pb(k: &VerifyKey) -> agent::SigningKey {
    agent::SigningKey {
        key_id: k.key_id.clone(),
        public_key: k.public.clone(),
        not_before: timestamp(k.not_before).into(),
        not_after: k.not_after.map(timestamp).into(),
        ..Default::default()
    }
}

fn enrolled_to_pb(e: &Enrolled) -> agent::EnrollResponse {
    agent::EnrollResponse {
        node: wire_id(IdKind::Node, e.node),
        assignment_keys: e.assignment_keys.iter().map(key_to_pb).collect(),
        token_keys: e.token_keys.iter().map(key_to_pb).collect(),
        workload_ipv4: e.workload_ipv4.map(|p| p.to_string()).unwrap_or_default(),
        ..Default::default()
    }
}

impl agent::AgentService for Api {
    async fn enroll(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, agent::EnrollRequest>,
    ) -> ServiceResult<agent::EnrollResponse> {
        let req = request.to_owned_message();
        let cmd = Enroll {
            token: req.enrollment_token,
            public_key: req.node_public_key,
            hostname: req.hostname,
            source_ip: ctx.peer_addr().map(|a| a.ip()),
        };
        let ip = cmd.source_ip;
        let enrolled = match self.app.enroll(cmd).await {
            Ok(enrolled) => enrolled,
            Err(e) => return Err(self.failed(e, (Attempt::Join, ip, None)).await),
        };
        Response::ok(enrolled_to_pb(&enrolled))
    }

    async fn session(
        &self,
        ctx: RequestContext,
        requests: InboundStream<agent::AgentMessage>,
    ) -> ServiceResult<ServiceStream<agent::ControlMessage>> {
        let rx = session::open(self, requests, ctx.peer_addr().map(|a| a.ip())).await?;
        let out = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|m| (Ok(m), rx))
        });
        Response::stream_ok(out)
    }
}
