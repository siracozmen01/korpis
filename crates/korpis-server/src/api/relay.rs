// SPDX-License-Identifier: AGPL-3.0-or-later

//! `RelayService` (§3.1 of docs/02-architecture.md): a client opens a session with a workload.
//! The relay verifies the token offline, as a relay without a database would: against the token
//! keys, the revocation list this process publishes, and the client's address, which this is
//! the first hop to see. Then it asks the node to carry the session and joins the two.

use super::wire::{Refuse as _, id, timestamp};
use super::{Api, error};
use crate::relay::Answer;
use connectrpc::ServiceStream;
use connectrpc::{ConnectError, InboundStream, RequestContext, Response, ServiceResult};
use futures_util::StreamExt as _;
use korpis_core::authz::token::{Client, verify};
use korpis_core::authz::{Kind, ObjectRef, Resource};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_request::Message as Request;
use pb::ErrorCode as C;
use std::time::Duration;
use tokio::sync::mpsc;

/// How many frames may wait for a slow reader, each way, before the faster side waits too.
pub(crate) const FRAMES: usize = 64;
/// How long a node has to carry or refuse a session it was asked to open.
const ANSWER: Duration = Duration::from_secs(10);

fn refused(code: &str, message: &str) -> ConnectError {
    match code {
        "refused" => error::session_refused(C::ERROR_CODE_DENIED, "session.refused", message),
        "not_here" => {
            error::session_refused(C::ERROR_CODE_UNSATISFIABLE, "session.not_here", message)
        }
        "unimplemented" => {
            error::session_refused(C::ERROR_CODE_UNSATISFIABLE, "session.unimplemented", message)
        }
        _ => unavailable(message),
    }
}

fn unavailable(message: &str) -> ConnectError {
    error::session_refused(C::ERROR_CODE_UNAVAILABLE, "session.unavailable", message)
}

/// Forwards what the client sends to the node until either side is done.
async fn client_to_node(
    mut requests: InboundStream<pb::RelayRequest>,
    to_node: mpsc::Sender<agent::CarryDown>,
) {
    while let Some(Ok(m)) = requests.next().await {
        let Some(Request::Frame(frame)) = m.to_owned_message().message else { continue };
        let down = agent::CarryDown { frame: (*frame).into(), ..Default::default() };
        if to_node.send(down).await.is_err() {
            return;
        }
    }
}

impl pb::RelayService for Api {
    async fn open(
        &self,
        ctx: RequestContext,
        mut requests: InboundStream<pb::RelayRequest>,
    ) -> ServiceResult<ServiceStream<pb::RelayResponse>> {
        let first = requests.next().await.ok_or_else(|| error::invalid("open", "required"))??;
        let Some(Request::Open(open)) = first.to_owned_message().message else {
            return Err(error::invalid("open", "the first message of a session opens it"));
        };
        let workload = id(IdKind::Workload, "open.workload", &open.workload).refuse()?;
        let node = id(IdKind::Node, "open.node", &open.node).refuse()?;
        let ip = ctx.peer_addr().map(|a| a.ip()).ok_or_else(|| unavailable("no client address"))?;
        let on = Resource::alone(ObjectRef::new(Kind::Workload, workload));
        let revoked = self.revocations.revoked();
        let at = (self.app.now(), Client::At(ip));
        let verified = verify(&open.token, &self.relay.keys(), &open.action, &on, &revoked, at)
            .map_err(|e| refused("refused", &e.to_string()))?;
        let asked = agent::OpenSession {
            token: open.token,
            action: open.action,
            workload: open.workload,
            params: open.params,
            ..Default::default()
        };
        let (to_client, mut rx) = mpsc::channel(FRAMES);
        let opened = pb::RelayOpened {
            expires_at: timestamp(verified.expires_at).into(),
            ..Default::default()
        };
        let (session, answered) = self
            .relay
            .ask(node, asked, (to_client, opened))
            .await
            .ok_or_else(|| unavailable("the node has no data connection to this relay"))?;
        let to_node = match tokio::time::timeout(ANSWER, answered).await {
            Ok(Ok(Answer::Carried(to_node))) => to_node,
            Ok(Ok(Answer::Refused { code, message })) => return Err(refused(&code, &message)),
            Ok(Err(_)) | Err(_) => {
                self.relay.forget(&session);
                return Err(unavailable("the node did not answer"));
            }
        };
        tracing::debug!(%session, %node, "session opened");
        tokio::spawn(client_to_node(requests, to_node));
        let out = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|m| m.map(Ok)));
        Response::stream_ok(out)
    }
}
