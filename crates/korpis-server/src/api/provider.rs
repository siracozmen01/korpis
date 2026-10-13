// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ProviderService` (§3.3 of docs/16-extensions.md): an installed extension dials out, registers
//! what it serves, and is called down the stream it opened. The registration lasts as long as the
//! stream; when either side ends it, the registration is withdrawn.

use super::wire::extension::provider_type_from_pb;
use super::wire::{Refuse, wire_id};
use super::{Api, ServerApp, error};
use crate::providers::Providers;
use connectrpc::{
    ConnectError, InboundStream, RequestContext, Response, ServiceResult, ServiceStream,
};
use futures_util::StreamExt as _;
use korpis_app::extension::Register;
use korpis_core::extension::Registration;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_control::Message as Down;
use korpis_proto::korpis::v1::__buffa::oneof::provider_message::Message as Up;
use std::sync::Arc;
use tokio::sync::mpsc;

/// How many messages may wait to go down one stream.
const OUTBOUND: usize = 64;

fn register_from_pb(r: &pb::Register) -> Result<Register, ConnectError> {
    Ok(Register {
        kind: provider_type_from_pb("register.type", r.r#type).refuse()?,
        name: r.name.clone(),
        version: r.version.clone(),
        concurrency: r.concurrency,
    })
}

fn registered(r: &Registration) -> pb::ProviderControl {
    let registered =
        pb::Registered { registration: wire_id(IdKind::Registration, r.id), ..Default::default() };
    pb::ProviderControl {
        message: Some(Down::Registered(Box::new(registered))),
        ..Default::default()
    }
}

/// Reads what comes up a registered stream until it ends, then withdraws the registration.
async fn uplink(
    app: Arc<ServerApp>,
    providers: Arc<Providers>,
    registration: Registration,
    mut requests: InboundStream<pb::ProviderMessage>,
) {
    while let Some(message) = requests.next().await {
        match message.map(|m| m.to_owned_message().message) {
            Ok(Some(Up::Result(r))) => {
                tracing::debug!(call = %r.call, "a provider answered a call nobody waits for");
            }
            Ok(Some(Up::Register(_))) => {
                tracing::warn!(registration = %registration.id, "a second Register; ending");
                break;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!(error = %e, "a provider stream failed");
                break;
            }
        }
    }
    providers.release(registration.id);
    if let Err(e) = app.withdraw_provider(&registration).await {
        tracing::warn!(error = %e, "a registration could not be withdrawn; it will expire");
    }
}

impl pb::ProviderService for Api {
    async fn serve(
        &self,
        ctx: RequestContext,
        mut requests: InboundStream<pb::ProviderMessage>,
    ) -> ServiceResult<ServiceStream<pb::ProviderControl>> {
        let caller = self.caller(&ctx).await?;
        let first =
            requests.next().await.ok_or_else(|| error::invalid("register", "required"))??;
        let Some(Up::Register(register)) = first.to_owned_message().message else {
            return Err(error::invalid("register", "the first message of a stream is a Register"));
        };
        let register = register_from_pb(&register)?;
        let replica = self.providers.replica();
        let registration = self
            .app
            .register_provider(&caller, replica, register)
            .await
            .map_err(error::from_app)?;
        let (tx, mut rx) = mpsc::channel(OUTBOUND);
        tx.send(registered(&registration)).await.map_err(|e| error::internal(&e))?;
        self.providers.hold(registration.clone(), tx);
        let (app, providers) = (Arc::clone(&self.app), Arc::clone(&self.providers));
        tokio::spawn(uplink(app, providers, registration, requests));
        let out = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|m| m.map(Ok)));
        Response::stream_ok(out)
    }
}
