// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a plan costs. Where a figure cannot be computed it is `Unknown`, never zero, and an
//! unknown impact is never auto-approvable (P4, §3.3 of docs/01-model.md).

use super::step::{Step, StepAction};
use crate::value::Value;
use crate::workload::spec::IntentBody;
use std::time::Duration;

/// A figure that may not be computable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Estimate<T> {
    /// Computed.
    Known(T),
    /// Not computable; the plan says so.
    Unknown,
}

impl<T> Estimate<T> {
    /// Whether it is known.
    pub fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }
}

/// How a plan changes what the workload reserves, signed: a plan can shrink a workload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceDelta {
    /// Reserved millicores.
    pub cpu_millis: i64,
    /// Reserved memory, in bytes.
    pub memory_bytes: i64,
    /// Reserved disk, in bytes.
    pub disk_bytes: i64,
    /// Volumes created.
    pub volumes: i64,
    /// Endpoints.
    pub endpoints: i64,
    /// Workloads.
    pub workloads: i64,
}

/// What a plan costs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Impact {
    /// Whether any step is disruptive.
    pub disruptive: bool,
    /// How long the workload is unavailable.
    pub downtime: Estimate<Duration>,
    /// Volumes whose data the plan deletes.
    pub data_at_risk: Estimate<Vec<String>>,
    /// How reservations change.
    pub cost_delta: ResourceDelta,
}

fn signed(after: u64, before: u64) -> i64 {
    let (a, b) = (i128::from(after), i128::from(before));
    i64::try_from(a - b).unwrap_or(if a > b { i64::MAX } else { i64::MIN })
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// How reservations change from `from` to `to`; a new workload counts itself.
#[must_use]
pub fn delta(from: Option<&IntentBody>, to: Option<&IntentBody>) -> ResourceDelta {
    let side = |b: Option<&IntentBody>| match b {
        None => [0; 6],
        Some(b) => [
            b.resources.cpu.reservation_millis,
            b.resources.memory.reservation,
            b.resources.disk.reservation,
            count(b.volumes.iter().filter(|v| v.existing.is_none()).count()),
            count(b.endpoints.len()),
            1,
        ],
    };
    let ([c1, m1, d1, v1, e1, w1], [c0, m0, d0, v0, e0, w0]) = (side(to), side(from));
    ResourceDelta {
        cpu_millis: signed(c1, c0),
        memory_bytes: signed(m1, m0),
        disk_bytes: signed(d1, d0),
        volumes: signed(v1, v0),
        endpoints: signed(e1, e0),
        workloads: signed(w1, w0),
    }
}

/// The impact of `steps`. A plan with no disruptive step causes no downtime; a disruptive one
/// has a downtime nobody has measured yet, so it is unknown.
#[must_use]
pub fn assess(steps: &[Step], cost_delta: ResourceDelta) -> Impact {
    let disruptive = steps.iter().any(|s| s.disruptive);
    let deleted: Vec<String> = steps
        .iter()
        .filter(|s| s.action == StepAction::DeleteVolume)
        .map(|s| s.detail.get("volume").and_then(Value::as_str).unwrap_or("?").to_owned())
        .collect();
    Impact {
        disruptive,
        downtime: if disruptive { Estimate::Unknown } else { Estimate::Known(Duration::ZERO) },
        data_at_risk: Estimate::Known(deleted),
        cost_delta,
    }
}
