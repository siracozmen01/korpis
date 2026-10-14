// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ProviderService` (§3.3 of docs/16-extensions.md): an installed extension dials out, registers
//! what it serves, and is called down the stream it opened. The registration lasts as long as the
//! stream; when either side ends it, the registration is withdrawn. Operators list what is
//! registered and ping it.

use super::wire::extension::provider_type_from_pb;
use super::wire::provider::{failure_to_pb, health_to_pb, outcome_from_pb, registration_to_pb};
use super::wire::{self, Refuse, wire_id};
use super::{Api, ServerApp, error};
use crate::providers::Providers;
use connectrpc::{
    ConnectError, InboundStream, RequestContext, Response, ServiceRequest, ServiceResult,
    ServiceStream,
};
use futures_util::StreamExt as _;
use korpis_app::extension::{CallTarget, Called, Register};
use korpis_core::extension::{CallOutcome, MAX_CALL_DEADLINE, Registration};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_control::Message as Down;
use korpis_proto::korpis::v1::__buffa::oneof::provider_message::Message as Up;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;

/// How long a ping waits when the request does not say.
const PING_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Records a provider's answer to a call sent down its stream.
async fn answered(
    app: &ServerApp,
    providers: &Providers,
    registration: uuid::Uuid,
    r: &pb::CallResult,
) {
    let Ok(call) = wire::id(IdKind::ProviderCall, "call", &r.call) else {
        tracing::debug!(call = %r.call, "a provider answered a call that is not one");
        return;
    };
    providers.settle(registration, call);
    match app.finish_provider_call(registration, call, &outcome_from_pb(r)).await {
        Ok(true) => {}
        Ok(false) => tracing::debug!(%call, "a provider answered a call nobody waits for"),
        Err(e) => tracing::warn!(error = %e, "recording a provider's answer failed"),
    }
}

fn target(org: &str, kind: pb::ProviderType, name: &str) -> Result<CallTarget, ConnectError> {
    Ok(CallTarget {
        organization: wire::id(IdKind::Organization, "organization", org).refuse()?,
        kind: provider_type_from_pb("type", kind.into()).refuse()?,
        name: name.to_owned(),
    })
}

fn pinged(c: &Called) -> pb::PingProviderResponse {
    let (registration, version) = c.sent.clone().unwrap_or_default();
    pb::PingProviderResponse {
        call: wire_id(IdKind::ProviderCall, c.id),
        registration: if c.sent.is_some() {
            wire_id(IdKind::Registration, registration)
        } else {
            String::new()
        },
        version,
        elapsed: wire::duration(c.elapsed).into(),
        error: match &c.outcome {
            CallOutcome::Failed(f) => {
                let mut e = failure_to_pb(f);
                let after = c.retry_at.and_then(|t| t.duration_since(SystemTime::now()).ok());
                e.retry_after = after.map(wire::duration).into();
                Some(e).into()
            }
            CallOutcome::Pong { .. }
            | CallOutcome::RecordsApplied { .. }
            | CallOutcome::StepResult(_)
            | CallOutcome::Access(_)
            | CallOutcome::Objects { .. }
            | CallOutcome::Deleted { .. }
            | CallOutcome::KeyMaterial { .. } => None.into(),
        },
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
            Ok(Some(Up::Result(r))) => answered(&app, &providers, registration.id, &r).await,
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
        // Room for every call it accepts at once and a cancellation of each, so sending to it
        // never waits: a stream with no room left fails the call rather than stalling others.
        let room = usize::try_from(registration.concurrency).unwrap_or(1).saturating_mul(2) + 1;
        let (tx, mut rx) = mpsc::channel(room);
        tx.send(registered(&registration)).await.map_err(|e| error::internal(&e))?;
        self.providers.hold(registration.clone(), tx);
        let (app, providers) = (Arc::clone(&self.app), Arc::clone(&self.providers));
        tokio::spawn(uplink(app, providers, registration, requests));
        let out = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|m| m.map(Ok)));
        Response::stream_ok(out)
    }

    async fn list_providers(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListProvidersRequest>,
    ) -> ServiceResult<pb::ListProvidersResponse> {
        let caller = self.caller(&ctx).await?;
        let org = wire::id(IdKind::Organization, "organization", request.organization).refuse()?;
        let listed = self.app.list_providers(&caller, org).await.map_err(error::from_app)?;
        let health = self.app.provider_health(&caller, org).await.map_err(error::from_app)?;
        Response::ok(pb::ListProvidersResponse {
            providers: listed.iter().map(registration_to_pb).collect(),
            health: health
                .iter()
                .map(|h| health_to_pb(h.kind, &h.name, &h.breaker, &h.measured))
                .collect(),
            ..Default::default()
        })
    }

    async fn ping_provider(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::PingProviderRequest>,
    ) -> ServiceResult<pb::PingProviderResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let kind = req.r#type.as_known().unwrap_or(pb::ProviderType::PROVIDER_TYPE_UNSPECIFIED);
        let target = target(&req.organization, kind, &req.name)?;
        let timeout = match req.timeout.as_option() {
            Some(d) => wire::duration_from("timeout", d).refuse()?.min(MAX_CALL_DEADLINE),
            None => PING_TIMEOUT,
        };
        let done =
            self.app.ping_provider(&caller, &target, timeout).await.map_err(error::from_app)?;
        Response::ok(pinged(&done))
    }
}
