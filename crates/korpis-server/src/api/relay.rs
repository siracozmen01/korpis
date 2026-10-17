// SPDX-License-Identifier: AGPL-3.0-or-later

//! `RelayService` (§3.1 of docs/02-architecture.md): a client opens a session with a workload.
//! The relay verifies the token offline, as a relay without a database does: against the token
//! keys, the revocation list it holds, and the client's address, which this is the first hop to
//! see. Then it asks the node to carry the session and joins the two.
//!
//! The same services serve in the control plane's process and in a relay's own, over a
//! [`Verifier`] that is the control plane's state in one and the feed's in the other.

use super::error;
use super::relay_watch::Watching;
use super::wire::{Refuse as _, id, timestamp};
use crate::relay::{Answer, Relay, Verifier};
use connectrpc::{ConnectError, InboundStream, RequestContext, Response, ServiceResult};
use connectrpc::{Router, ServiceStream};
use futures_util::StreamExt as _;
use korpis_core::authz::token::{Client, KeyRing, Verified, verify};
use korpis_core::authz::{Kind, ObjectRef, Resource};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_request::Message as Request;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use pb::ErrorCode as C;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;

/// How many frames may wait for a slow reader, each way, before the faster side waits too.
pub(crate) const FRAMES: usize = 64;
/// How long a node has to carry or refuse a session it was asked to open.
const ANSWER: Duration = Duration::from_secs(10);

pub(super) fn refused(code: &str, message: &str) -> ConnectError {
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

pub(super) fn unavailable(message: &str) -> ConnectError {
    error::session_refused(C::ERROR_CODE_UNAVAILABLE, "session.unavailable", message)
}

/// The relay role's services, `RelayService` and `DataService`, verifying by `V`.
pub struct RelayRole<V> {
    pub(super) relay: Arc<Relay>,
    pub(super) verifier: Arc<V>,
    /// The sessions opened with `Watch`, which `Send` delivers to.
    pub(super) watching: Arc<Watching<V>>,
}

impl<V> std::fmt::Debug for RelayRole<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayRole").field("relay", &self.relay).finish_non_exhaustive()
    }
}

impl<V: Verifier> RelayRole<V> {
    /// The role, verifying by `verifier`.
    #[must_use]
    pub fn new(verifier: Arc<V>) -> Self {
        Self { relay: Arc::new(Relay::default()), verifier, watching: Arc::default() }
    }

    /// What it verifies by.
    #[must_use]
    pub fn verifier(&self) -> &V {
        &self.verifier
    }

    /// Registers its services on `router`.
    #[must_use]
    pub fn register(self: Arc<Self>, router: Router) -> Router {
        let router = pb::RelayServiceExt::register(Arc::clone(&self), router);
        agent::DataServiceExt::register(self, router)
    }
}

/// What the relay judges a session's tokens by: the one it opened with, and every renewal.
pub(super) struct Judge<V> {
    keys: KeyRing,
    verifier: Arc<V>,
    action: String,
    on: Resource,
    client: Client,
    /// Who opened the session; a renewal must be theirs. None until the first token verifies.
    subject: Option<String>,
}

impl<V: Verifier> Judge<V> {
    pub(super) fn judge(&self, token: &str) -> Result<Verified, String> {
        self.judge_from(token, self.client)
    }

    /// [`Self::judge`], for a token presented by `client`: a session reached by more than one
    /// request (`Send`) has its token's conditions on the client's address checked for each.
    pub(super) fn judge_from(&self, token: &str, client: Client) -> Result<Verified, String> {
        let revoked = self.verifier.revoked();
        let at = (SystemTime::now(), client);
        let verified = verify(token, &self.keys, &self.action, &self.on, &revoked, at)
            .map_err(|e| e.to_string())?;
        if self.subject.as_ref().is_some_and(|s| *s != verified.subject) {
            return Err("a renewal must be the same subject's".to_owned());
        }
        Ok(verified)
    }
}

pub(super) fn end(code: &str, message: String) -> agent::CarryDown {
    let end = pb::SessionEnd { code: code.to_owned(), message, ..Default::default() };
    let frame = pb::SessionFrame { frame: Some(Frame::End(Box::new(end))), ..Default::default() };
    agent::CarryDown { frame: frame.into(), ..Default::default() }
}

/// Forwards what the client sends to the node until either side is done. A renewal is judged
/// here first, as the token it renews was: one the relay refuses ends the session, with the
/// reason, rather than reaching the node.
async fn client_to_node<V: Verifier>(
    mut requests: InboundStream<pb::RelayRequest>,
    to_node: mpsc::Sender<agent::CarryDown>,
    judge: Judge<V>,
) {
    while let Some(Ok(m)) = requests.next().await {
        let Some(Request::Frame(frame)) = m.to_owned_message().message else { continue };
        let down = match &frame.frame {
            Some(Frame::Renew(token)) => match judge.judge(token) {
                Ok(_) => agent::CarryDown { frame: (*frame).into(), ..Default::default() },
                Err(e) => {
                    let _ = to_node.send(end("refused", e)).await;
                    return;
                }
            },
            _ => agent::CarryDown { frame: (*frame).into(), ..Default::default() },
        };
        if to_node.send(down).await.is_err() {
            return;
        }
    }
}

/// A session the node agreed to carry: where the client's frames go, where the node's come
/// from, and what judges the session's renewals.
pub(super) struct Carried<V> {
    pub(super) session: String,
    pub(super) to_node: mpsc::Sender<agent::CarryDown>,
    pub(super) from_node: mpsc::Receiver<pb::RelayResponse>,
    pub(super) judge: Judge<V>,
}

impl<V: Verifier> RelayRole<V> {
    /// Opens `open` for the client of `ctx`: the token verified offline against its address,
    /// the node found, asked, and its answer awaited. `send_key` goes to the client in the
    /// `opened` answer; empty for a session opened with `Open`.
    pub(super) async fn carry(
        &self,
        ctx: &RequestContext,
        open: pb::RelayOpen,
        send_key: String,
    ) -> Result<Carried<V>, ConnectError> {
        let workload = id(IdKind::Workload, "open.workload", &open.workload).refuse()?;
        let ip = ctx.peer_addr().map(|a| a.ip()).ok_or_else(|| unavailable("no client address"))?;
        let mut judge = Judge {
            keys: self.verifier.keys(),
            verifier: Arc::clone(&self.verifier),
            action: open.action.clone(),
            on: Resource::alone(ObjectRef::new(Kind::Workload, workload)),
            client: Client::At(ip),
            subject: None,
        };
        let verified = judge.judge(&open.token).map_err(|e| refused("refused", &e))?;
        judge.subject = Some(verified.subject);
        // The gateway sealed the node it is placed on into the route; a client that may read
        // the node can name it instead.
        let node = if open.route.is_empty() {
            id(IdKind::Node, "open.node", &open.node).refuse()?
        } else {
            self.verifier
                .routes()
                .open(workload, &open.route)
                .map_err(|e| error::invalid("open.route", e.to_string()))?
        };
        let asked = agent::OpenSession {
            token: open.token,
            action: open.action,
            workload: open.workload,
            params: open.params,
            ..Default::default()
        };
        let (to_client, from_node) = mpsc::channel(FRAMES);
        let opened = pb::RelayOpened {
            expires_at: timestamp(verified.expires_at).into(),
            send_key,
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
        Ok(Carried { session, to_node, from_node, judge })
    }
}

impl<V: Verifier> pb::RelayService for RelayRole<V> {
    async fn open(
        &self,
        ctx: RequestContext,
        mut requests: InboundStream<pb::RelayRequest>,
    ) -> ServiceResult<ServiceStream<pb::RelayResponse>> {
        let first = requests.next().await.ok_or_else(|| error::invalid("open", "required"))??;
        let Some(Request::Open(open)) = first.to_owned_message().message else {
            return Err(error::invalid("open", "the first message of a session opens it"));
        };
        let carried = self.carry(&ctx, *open, String::new()).await?;
        tokio::spawn(client_to_node(requests, carried.to_node, carried.judge));
        let mut rx = carried.from_node;
        let out = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|m| m.map(Ok)));
        Response::stream_ok(out)
    }

    async fn watch(
        &self,
        ctx: RequestContext,
        request: connectrpc::ServiceRequest<'_, pb::RelayOpen>,
    ) -> ServiceResult<ServiceStream<pb::RelayResponse>> {
        self.watch_session(ctx, request.to_owned_message()).await
    }

    async fn send(
        &self,
        ctx: RequestContext,
        request: connectrpc::ServiceRequest<'_, pb::SendRequest>,
    ) -> ServiceResult<pb::SendResponse> {
        Response::ok(self.deliver(&ctx, request.to_owned_message()).await?)
    }
}
