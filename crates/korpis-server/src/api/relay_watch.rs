// SPDX-License-Identifier: AGPL-3.0-or-later

//! A session for a client that cannot stream both ways ("From a browser" in §3.1 of
//! docs/02-architecture.md): `Watch` opens it as `Open` does and streams the node's frames back,
//! and `Send` delivers the client's frames, numbered, in order and once. The session lives as
//! long as its `Watch` stream: when the client stops reading, or the node ends it, it is
//! forgotten, and what was sending into it is dropped, which ends the node's side too.
//!
//! `Send` is authorized by the key the `opened` answer gave only the watcher. The session
//! identifier is for logs and is no secret; the key is never logged, and a key that does not
//! match is answered exactly as a session this process does not hold, `session.not_here`. And
//! each `Send` is a request of its own, from an address of its own, so the session's current
//! token is judged again for it: a token whose grant limits the client's address
//! (`source_cidr`) holds for every frame sent, as it holds for every frame of an `Open` stream,
//! whose one connection has one address.

use super::error;
use super::relay::{Carried, Judge, RelayRole, end, refused, unavailable};
use crate::relay::Verifier;
use connectrpc::{ConnectError, RequestContext, Response, ServiceResult, ServiceStream};
use korpis_core::authz::token::Client;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use pb::ErrorCode as C;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::mpsc;

/// The sessions this process holds for `Watch`, by session identifier.
pub struct Watching<V> {
    sessions: Mutex<HashMap<String, Arc<Watched<V>>>>,
}

impl<V> Default for Watching<V> {
    fn default() -> Self {
        Self { sessions: Mutex::new(HashMap::new()) }
    }
}

impl<V> Watching<V> {
    // Every operation changes one entry with no await inside, so a poisoned map is consistent.
    fn guard(&self) -> MutexGuard<'_, HashMap<String, Arc<Watched<V>>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn forget(&self, session: &str) {
        self.guard().remove(session);
    }
}

/// One watched session: its key, and the client's frames delivered so far.
struct Watched<V> {
    key: String,
    feed: tokio::sync::Mutex<Feed<V>>,
}

/// Where the client's frames go, how many have gone, the token the session holds now, and what
/// judges it.
struct Feed<V> {
    delivered: u64,
    token: String,
    to_node: mpsc::Sender<agent::CarryDown>,
    judge: Judge<V>,
}

impl<V: Verifier> Feed<V> {
    /// Takes the renewal `token`, sent by `client`, as the token judged from now on; one the
    /// relay refuses ends the session with the reason, as `Open` ends it, before the node hears
    /// of it.
    async fn renew(&mut self, token: &str, client: Client) -> Result<(), String> {
        if let Err(e) = self.judge.judge_from(token, client) {
            let _ = self.to_node.send(end("refused", e.clone())).await;
            return Err(e);
        }
        self.token = token.to_owned();
        Ok(())
    }
}

/// Forgets the session when the `Watch` stream holding it is dropped.
struct Forget<V> {
    watching: Arc<Watching<V>>,
    session: String,
}

impl<V> Drop for Forget<V> {
    fn drop(&mut self) {
        self.watching.forget(&self.session);
    }
}

/// A key only the watcher is told: 32 random bytes, in hex.
fn send_key() -> Result<String, ConnectError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| error::internal(&e))?;
    Ok(bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// Whether `a` and `b` are equal, in time that does not depend on where they differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn not_here() -> ConnectError {
    error::session_refused(
        C::ERROR_CODE_UNSATISFIABLE,
        "session.not_here",
        "this process holds no such session: open it again",
    )
}

impl<V: Verifier> RelayRole<V> {
    /// `Watch`: opens the session and streams the node's frames back.
    pub(super) async fn watch_session(
        &self,
        ctx: RequestContext,
        open: pb::RelayOpen,
    ) -> ServiceResult<ServiceStream<pb::RelayResponse>> {
        let key = send_key()?;
        let token = open.token.clone();
        let Carried { session, to_node, mut from_node, judge } =
            self.carry(&ctx, open, key.clone()).await?;
        let feed = tokio::sync::Mutex::new(Feed { delivered: 0, token, to_node, judge });
        self.watching.guard().insert(session.clone(), Arc::new(Watched { key, feed }));
        let forget = Forget { watching: Arc::clone(&self.watching), session };
        let out = futures_util::stream::poll_fn(move |cx| {
            let _held = &forget;
            from_node.poll_recv(cx).map(|m| m.map(Ok))
        });
        Response::stream_ok(out)
    }

    /// `Send`: delivers the frames of `request` the session has not had yet, in order.
    pub(super) async fn deliver(
        &self,
        ctx: &RequestContext,
        request: pb::SendRequest,
    ) -> Result<pb::SendResponse, ConnectError> {
        let watched = self.watching.guard().get(&request.session).cloned();
        let watched = watched.filter(|w| same(&w.key, &request.send_key)).ok_or_else(not_here)?;
        let ip = ctx.peer_addr().map(|a| a.ip()).ok_or_else(|| unavailable("no client address"))?;
        let client = Client::At(ip);
        let mut feed = watched.feed.lock().await;
        if let Err(e) = feed.judge.judge_from(&feed.token, client) {
            return Err(refused("refused", &e));
        }
        if request.first > feed.delivered {
            let message = format!("frames from {} were never delivered", feed.delivered);
            return Err(error::invalid("first", message));
        }
        let skip = usize::try_from(feed.delivered - request.first).unwrap_or(usize::MAX);
        for frame in request.frames.into_iter().skip(skip) {
            if let Some(Frame::Renew(token)) = &frame.frame
                && let Err(e) = feed.renew(token, client).await
            {
                self.watching.forget(&request.session);
                return Err(refused("refused", &e));
            }
            let down = agent::CarryDown { frame: frame.into(), ..Default::default() };
            if feed.to_node.send(down).await.is_err() {
                return Err(unavailable("the session has ended"));
            }
            feed.delivered += 1;
        }
        Ok(pb::SendResponse { delivered: feed.delivered, ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_key_is_long_random_and_compared_whole() {
        let (a, b) = (send_key().unwrap(), send_key().unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(same(&a, &a.clone()));
        assert!(!same(&a, &b));
        assert!(!same(&a, &a[..63]), "a prefix is not the key");
    }
}
