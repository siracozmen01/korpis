// SPDX-License-Identifier: AGPL-3.0-or-later

//! Executing a plan's node steps (§4.8 and §4.9 of docs/02-architecture.md): in order, the next
//! one not yet executed, never past `through`. Each completion is journalled, keyed by plan and
//! step, before it is kept as done, and a failure is journalled and stops the plan here: the
//! control plane fails it, and the next assignment no longer carries it.

use super::act::{Failure, volume_id};
use super::progress::Step;
use super::{Reconciler, Tracked, effect, text};
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::value::Kind;
use std::collections::BTreeMap;

/// What a step leaves behind, for its effect.
type After = BTreeMap<String, pb::Value>;

fn detail<'a>(s: &'a pb::Step, key: &str) -> Option<&'a str> {
    match s.detail.get(key)?.kind.as_ref()? {
        Kind::StringValue(v) => Some(v.as_str()),
        _ => None,
    }
}

fn number(s: &pb::Step, key: &str) -> u64 {
    match s.detail.get(key).and_then(|v| v.kind.as_ref()) {
        Some(Kind::IntValue(n)) => u64::try_from(*n).unwrap_or(0),
        _ => 0,
    }
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Executes what is outstanding of `plan`; whether nothing is left that the node may do.
    pub(super) async fn steps(
        &mut self,
        a: &agent::Assignment,
        plan: &agent::NodePlan,
        t: &mut Tracked,
    ) -> bool {
        let done = self.progress.of(&a.workload).step.filter(|s| s.plan == plan.plan);
        if done.as_ref().is_some_and(|s| s.failed) {
            return false;
        }
        let after = done.map(|s| s.index);
        let outstanding: Vec<&pb::Step> = plan
            .steps
            .iter()
            .filter(|s| after.is_none_or(|i| s.index > i) && s.index <= plan.through)
            .collect();
        for s in outstanding {
            let result = self.execute(a, s, t).await;
            let ok = result.is_ok();
            let mut e = effect(Some(&a.workload), &format!("step.{}", s.action), ok);
            e.plan = Some(plan.plan.clone());
            e.step = Some(s.index);
            match result {
                Ok(left) => e.after = left.into_iter().collect(),
                Err(Failure(why)) => {
                    let error = pb::Error {
                        reason: "step.failed".into(),
                        message: why,
                        ..Default::default()
                    };
                    e.error = error.into();
                }
            }
            if !self.record(e).await {
                return false;
            }
            let kept = Step { plan: plan.plan.clone(), index: s.index, failed: !ok };
            if let Err(e) = self.progress.stepped(&a.workload, kept) {
                tracing::error!(error = %e, "progress could not be kept; the step runs again");
                return false;
            }
            if !ok {
                return false;
            }
        }
        plan.steps.iter().all(|s| s.index <= plan.through)
    }

    /// Executes one step.
    async fn execute(
        &mut self,
        a: &agent::Assignment,
        s: &pb::Step,
        t: &mut Tracked,
    ) -> Result<After, Failure> {
        let mut after = After::new();
        match s.action.as_str() {
            "pull_recipe" => {
                let spec = self.prepare(a, t).await?;
                self.host.fetch(&spec.rootfs).await?;
                after.insert("rootfs".into(), text(&spec.rootfs));
            }
            "create_volume" | "attach_volume" => {
                let name = detail(s, "volume").unwrap_or_default();
                let request = helper::EnsureVolumeRequest {
                    volume: volume_id(&a.workload, name, detail(s, "existing")),
                    workload: a.workload.clone(),
                    storage_class: detail(s, "storage_class").unwrap_or_default().to_owned(),
                    size_bytes: number(s, "size_bytes"),
                    ..Default::default()
                };
                let made = self.host.ensure_volume(&request).await?;
                after.insert("volume".into(), text(&request.volume));
                after.insert("created".into(), text(if made.created { "true" } else { "false" }));
            }
            // A node-local volume is mounted by the runtime object, so it is detached when the
            // object is next rebuilt or destroyed; its data is retained.
            "detach_volume" => {
                after.insert("applies_at".into(), text("the next rebuild of the runtime object"));
            }
            "delete_volume" => {
                let name = detail(s, "volume").unwrap_or_default();
                self.host
                    .delete_volume(&volume_id(&a.workload, name, detail(s, "existing")))
                    .await?;
            }
            "create" => {
                let id = self.current(a, t).await?;
                after.insert("runtime_id".into(), text(&id));
            }
            "start" => {
                let id = self.start(a, t).await?;
                after.insert("runtime_id".into(), text(&id));
            }
            "stop" => self.stop(t).await?,
            "restart" => {
                let id = self.restart(a, t).await?;
                after.insert("runtime_id".into(), text(&id));
            }
            // Validated now, applied at the next start: the object is rebuilt from the target.
            "reconfigure" => {
                self.prepare(a, t).await?;
                after.insert("applies_at".into(), text("the next start"));
            }
            "reconfigure_network" => {
                after.insert("applies_at".into(), text("the next start"));
            }
            "retire" => self.destroy(t).await?,
            other => return Err(Failure(format!("a node does not execute {other:?}"))),
        }
        Ok(after)
    }
}
