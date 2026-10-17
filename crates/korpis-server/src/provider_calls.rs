// SPDX-License-Identifier: AGPL-3.0-or-later

//! This replica's side of the provider call queue (§3.3 of docs/16-extensions.md): it claims
//! the calls the streams it holds may take, sends them down, cancels those still unanswered at
//! their deadline, confirms the registrations it holds, and withdraws those of replicas gone.

use crate::api::Api;
use korpis_app::ports::Wakeups as _;
use korpis_core::extension::{CallFailure, CallOutcome, ProviderCall, Registration};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_control::Message as Down;
use korpis_wire::provider::call_to_pb;
use korpis_wire::wire_id;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// How often this replica confirms the provider streams it holds.
const REGISTRATIONS_SEEN: Duration = Duration::from_secs(5);

/// Claims and sends, whenever a call is queued or finished (and every second regardless), the
/// calls each stream this replica holds may take. One task claims for every stream, so a
/// stream's free slots are never counted by two claims at once. The task ends with the process.
pub(crate) async fn dispatch(api: Arc<Api>) {
    let mut wakeups = api.app().provider_call_wakeups();
    loop {
        for registration in api.providers().held() {
            claim(&api, &registration).await;
        }
        if wakeups.next().await.is_none() {
            return;
        }
    }
}

/// Claims and sends the calls one held stream may take now.
async fn claim(api: &Arc<Api>, registration: &Registration) {
    match api.app().claim_provider_calls(registration).await {
        Ok(calls) => {
            for call in calls {
                send(api, registration, &call).await;
            }
        }
        Err(e) => tracing::warn!(error = %e, "claiming provider calls failed"),
    }
}

/// Sends one claimed call down its stream and arms its cancellation; a stream that is gone or
/// full fails the call as retryable at once.
async fn send(api: &Arc<Api>, registration: &Registration, call: &ProviderCall) {
    let down = pb::ProviderControl {
        message: Some(Down::Call(Box::new(call_to_pb(call)))),
        ..Default::default()
    };
    if !api.providers().send_call(registration.id, call.id, down) {
        let ended = CallOutcome::Failed(CallFailure::stream_ended());
        if let Err(e) = api.app().finish_provider_call(registration.id, call.id, &ended).await {
            tracing::warn!(error = %e, "failing an unsendable provider call failed");
        }
        return;
    }
    let (api, registration, id) = (Arc::clone(api), registration.id, call.id);
    let left = call.deadline.duration_since(SystemTime::now()).unwrap_or_default();
    tokio::spawn(async move {
        tokio::time::sleep(left).await;
        let cancel =
            pb::CancelCall { call: wire_id(IdKind::ProviderCall, id), ..Default::default() };
        let down = pb::ProviderControl {
            message: Some(Down::Cancel(Box::new(cancel))),
            ..Default::default()
        };
        api.providers().cancel(registration, id, down);
    });
}

/// Confirms, every few seconds, the provider streams this replica holds, closing any whose
/// registration was withdrawn elsewhere; withdraws registrations no replica has confirmed for
/// six times as long, since their replica is gone; and fails the calls nobody can answer any
/// more. The task ends with the process.
pub(crate) async fn keep_registrations(api: Arc<Api>) {
    let mut tick = tokio::time::interval(REGISTRATIONS_SEEN);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let before = api.providers().held_ids();
        match api.app().keep_registrations(api.providers().replica()).await {
            Ok(alive) => api.providers().close_withdrawn(&before, &alive),
            Err(e) => tracing::warn!(error = %e, "confirming provider streams failed"),
        }
        match api.app().expire_registrations(REGISTRATIONS_SEEN * 6).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(registrations = n, "provider registrations of a lost replica"),
            Err(e) => tracing::warn!(error = %e, "expiring provider registrations failed"),
        }
        if let Err(e) = api.app().sweep_provider_calls().await {
            tracing::warn!(error = %e, "sweeping provider calls failed");
        }
    }
}
