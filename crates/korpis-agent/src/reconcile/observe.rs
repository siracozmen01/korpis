// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the node reports about each workload (§4.3 of docs/02-architecture.md): field by field,
//! what it has converged to, and what it sees. An observation is sent when it changes and again
//! on every new session; the control plane keeps the latest and decides from it whether the
//! workload has converged.

use super::act::running;
use super::workload::wants_running;
use super::{Reconciler, Tracked};
use crate::host::Host;
use crate::runtime::Runtime;
use buffa_types::google::protobuf::Timestamp;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::time::{SystemTime, UNIX_EPOCH};

fn timestamp(t: SystemTime) -> Timestamp {
    let since = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    Timestamp {
        seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(since.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    }
}

/// The state the node sees.
fn state(a: &agent::Assignment, t: &Tracked) -> pb::ObservedState {
    if t.rejected.is_some() {
        return pb::ObservedState::OBSERVED_STATE_UNSATISFIABLE;
    }
    let wanted = wants_running(a);
    match t.status.as_ref().and_then(|s| s.state.as_known()) {
        Some(driver::RuntimeState::RUNTIME_STATE_RUNNING) => {
            pb::ObservedState::OBSERVED_STATE_RUNNING
        }
        Some(driver::RuntimeState::RUNTIME_STATE_STOPPED) if wanted && !t.stopped_by_us => {
            pb::ObservedState::OBSERVED_STATE_CRASHED
        }
        _ if t.runtime.is_none() && wanted && !a.retire.is_set() => {
            pb::ObservedState::OBSERVED_STATE_STARTING
        }
        Some(driver::RuntimeState::RUNTIME_STATE_CREATED) if wanted => {
            pb::ObservedState::OBSERVED_STATE_STARTING
        }
        _ => pb::ObservedState::OBSERVED_STATE_STOPPED,
    }
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// What the node observes of `a` now.
    fn observation(&self, a: &agent::Assignment, t: &mut Tracked) -> pb::Observation {
        let state = state(a, t);
        let at_power = if wants_running(a) {
            state == pb::ObservedState::OBSERVED_STATE_RUNNING
        } else {
            state == pb::ObservedState::OBSERVED_STATE_STOPPED
        };
        if at_power && t.generation == a.generation {
            t.power_version = a.power.as_option().map_or(0, |p| p.version);
        }
        let done = self.progress.of(&a.workload);
        let cycle = done.cycle.or_else(|| t.labels.as_ref().map(|l| l.power_cycle));
        let plan = done.step.filter(|s| !s.failed).map(|s| pb::PlanProgress {
            plan: s.plan,
            step: s.index,
            ..Default::default()
        });
        let rejected = t.rejected.as_ref();
        pb::Observation {
            workload: a.workload.clone(),
            node: self.wiring.node.clone(),
            assignment_seen: t.generation,
            intent_seen: t.labels.as_ref().map(|l| l.intent.clone()).unwrap_or_default(),
            power_seen: pb::PowerSeen {
                version: t.power_version,
                cycle: cycle.unwrap_or_default(),
                ..Default::default()
            }
            .into(),
            plan_progress: plan.into(),
            state: state.into(),
            reason: rejected.map(|r| r.reason).unwrap_or_default(),
            reason_detail: rejected.map(|r| r.detail.clone()).unwrap_or_default(),
            exit_code: t.status.as_ref().filter(|s| !running(s)).and_then(|s| s.exit_code),
            runtime_id: t.runtime.clone().unwrap_or_default(),
            ..Default::default()
        }
    }

    /// Sends the observation of `a` if it changed since the last one sent.
    pub(super) async fn observe(&self, a: &agent::Assignment, t: &mut Tracked) {
        let mut o = self.observation(a, t);
        let changed_state = t.sent.as_ref().is_none_or(|s| s.state != o.state);
        if changed_state || t.since.is_none() {
            t.since = Some(SystemTime::now());
        }
        o.since = t.since.map(timestamp).into();
        if t.sent.as_ref() == Some(&o) {
            return;
        }
        t.sent = Some(o.clone());
        o.observed_at = timestamp(SystemTime::now()).into();
        let m = agent::AgentMessage {
            message: Some(Up::Observation(Box::new(o))),
            ..Default::default()
        };
        if self.wiring.outbound.send(m).await.is_err() {
            tracing::debug!("the session is gone; the observation goes with the next one");
        }
    }
}
