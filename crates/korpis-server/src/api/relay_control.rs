// SPDX-License-Identifier: AGPL-3.0-or-later

//! `RelayControlService` (§3.1 of docs/02-architecture.md): what a relay running as its own
//! process asks of the control plane. Joining is authenticated by the enrollment token in the
//! request, as an agent's enrollment is; the feed and renewals by the relay's identity key.
//!
//! The feed sends the terms first, then each part whole when the relay connects and again when it
//! changes: the revocation list as this process publishes it, and the token and node keys as a
//! look every [`LOOK`] finds them; a look that sent nothing sends a heartbeat, so the relay can
//! tell a quiet feed from a lost one. It ends when the relay is removed, so a removed relay stops
//! verifying by anything newer than its last list.

use super::agent::{key_to_pb, proof_from_pb};
use super::wire::{self, Refuse as _, timestamp, wire_id};
use super::{Api, ServerApp, error};
use crate::revocations::Revocations;
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream};
use korpis_app::relay::JoinRelay;
use korpis_core::audit::failure::Attempt;
use korpis_core::id::IdKind;
use korpis_core::relay::SEEN_EVERY;
use korpis_proto::korpis::relay::v1 as relay;
use korpis_proto::korpis::relay::v1::__buffa::oneof::feed_update::Message as Update;
use korpis_wire::duration;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

/// How often the feed looks for changed keys, and for the relay's removal.
const LOOK: Duration = Duration::from_secs(2);
/// The longest the feed goes without sending, as the relay is told: a look sends at least a
/// heartbeat, and a look takes [`LOOK`] and the time its reads take.
const HEARTBEAT: Duration = Duration::from_secs(5);

fn update(message: Update) -> relay::FeedUpdate {
    relay::FeedUpdate { message: Some(message), ..Default::default() }
}

fn node_keys(keys: &[(Uuid, Vec<u8>)]) -> relay::NodeKeys {
    let nodes = keys
        .iter()
        .map(|(node, key)| relay::NodeKey {
            node: wire_id(IdKind::Node, *node),
            public_key: key.clone(),
            ..Default::default()
        })
        .collect();
    relay::NodeKeys { nodes, ..Default::default() }
}

/// The keys as they stand, each sent when it differs from what was sent last; whether any was.
async fn keys(
    app: &ServerApp,
    sent: &mut (Option<relay::TokenKeys>, Option<relay::NodeKeys>),
    tx: &mpsc::Sender<relay::FeedUpdate>,
) -> Result<bool, String> {
    let mut any = false;
    let feed = app.relay_feed().await.map_err(|e| e.to_string())?;
    let route_keys = feed.route_keys.iter().map(|k| relay::RouteKey {
        key_id: k.key_id,
        key: k.bytes().to_vec(),
        ..Default::default()
    });
    let tokens = relay::TokenKeys {
        keys: feed.token_keys.iter().map(key_to_pb).collect(),
        route_keys: route_keys.collect(),
        ..Default::default()
    };
    if sent.0.as_ref() != Some(&tokens) {
        tx.send(update(Update::TokenKeys(Box::new(tokens.clone()))))
            .await
            .map_err(|e| e.to_string())?;
        (sent.0, any) = (Some(tokens), true);
    }
    let nodes = node_keys(&feed.node_keys);
    if sent.1.as_ref() != Some(&nodes) {
        tx.send(update(Update::NodeKeys(Box::new(nodes.clone()))))
            .await
            .map_err(|e| e.to_string())?;
        (sent.1, any) = (Some(nodes), true);
    }
    Ok(any)
}

fn terms(app: &ServerApp) -> relay::FeedUpdate {
    update(Update::Terms(Box::new(relay::FeedTerms {
        auth_skew: duration(app.auth_skew()).into(),
        heartbeat_interval: duration(HEARTBEAT).into(),
        ..Default::default()
    })))
}

fn heartbeat(app: &ServerApp) -> relay::FeedUpdate {
    let beat = relay::Heartbeat { sent_at: timestamp(app.now()).into(), ..Default::default() };
    update(Update::Heartbeat(Box::new(beat)))
}

/// Feeds `relay` until it is removed or goes away.
async fn feed(
    app: Arc<ServerApp>,
    revocations: Arc<Revocations>,
    relay: Uuid,
    tx: mpsc::Sender<relay::FeedUpdate>,
) {
    let mut lists = revocations.subscribe();
    let mut sent = (None, None);
    let mut list_sent = false;
    let mut seen: Option<tokio::time::Instant> = None;
    if tx.send(terms(&app)).await.is_err() {
        return;
    }
    loop {
        if !matches!(app.relay_joined(relay).await, Ok(true)) {
            tracing::info!(%relay, "relay feed ended: removed");
            return;
        }
        if seen.is_none_or(|at| at.elapsed() >= SEEN_EVERY) {
            match app.relay_seen(relay).await {
                Ok(()) => seen = Some(tokio::time::Instant::now()),
                // The next look records it; ListRelays shows the relay as quiet meanwhile.
                Err(e) => tracing::warn!(%relay, error = %e, "recording the relay as seen failed"),
            }
        }
        let mut sent_any = match keys(&app, &mut sent, &tx).await {
            Ok(any) => any,
            Err(e) => {
                tracing::warn!(%relay, error = %e, "relay feed ended");
                return;
            }
        };
        let current = (!list_sent || lists.has_changed().unwrap_or(false))
            .then(|| lists.borrow_and_update().clone())
            .flatten();
        if let Some(list) = current {
            if tx.send(update(Update::RevocationList(Box::new((*list).clone())))).await.is_err() {
                return;
            }
            (list_sent, sent_any) = (true, true);
        }
        if !sent_any && tx.send(heartbeat(&app)).await.is_err() {
            return;
        }
        tokio::select! {
            _ = lists.changed() => {}
            () = tokio::time::sleep(LOOK) => {}
            () = tx.closed() => return,
        }
    }
}

impl relay::RelayControlService for Api {
    async fn join(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, relay::JoinRequest>,
    ) -> ServiceResult<relay::JoinResponse> {
        let req = request.to_owned_message();
        let cmd = JoinRelay {
            token: req.enrollment_token,
            identity_key: req.identity_key,
            tls_key: req.tls_key,
            source_ip: ctx.peer_addr().map(|a| a.ip()),
        };
        let ip = cmd.source_ip;
        let joined = match self.app.join_relay(cmd).await {
            Ok(joined) => joined,
            Err(e) => return Err(self.failed(e, (Attempt::RelayJoin, ip, None)).await),
        };
        Response::ok(relay::JoinResponse {
            relay: wire_id(IdKind::Relay, joined.relay.id),
            name: joined.relay.name.as_str().to_owned(),
            url: joined.relay.url.as_str().to_owned(),
            certificate_chain_pem: joined.certificate.chain_pem,
            certificate_expires_at: timestamp(joined.certificate.expires_at).into(),
            authority_pem: joined.authority_pem,
            token_keys: joined.token_keys.iter().map(key_to_pb).collect(),
            ..Default::default()
        })
    }

    async fn feed(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, relay::FeedRequest>,
    ) -> ServiceResult<ServiceStream<relay::FeedUpdate>> {
        let req = request.to_owned_message();
        let id = wire::id(IdKind::Relay, "relay", &req.relay).refuse()?;
        let proof = proof_from_pb(&req.relay, req.auth.as_option())?;
        self.app.authenticate_relay(id, &proof).await.map_err(error::from_app)?;
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(feed(Arc::clone(&self.app), Arc::clone(&self.revocations), id, tx));
        let out = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|m| m.map(Ok)));
        Response::stream_ok(out)
    }

    async fn renew(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, relay::RenewRequest>,
    ) -> ServiceResult<relay::RenewResponse> {
        let req = request.to_owned_message();
        let id = wire::id(IdKind::Relay, "relay", &req.relay).refuse()?;
        let proof = proof_from_pb(&req.relay, req.auth.as_option())?;
        let issued =
            self.app.renew_relay(id, &proof, &req.tls_key).await.map_err(error::from_app)?;
        Response::ok(relay::RenewResponse {
            certificate_chain_pem: issued.chain_pem,
            certificate_expires_at: timestamp(issued.expires_at).into(),
            ..Default::default()
        })
    }
}
