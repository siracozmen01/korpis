// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node reports of the runs it executes (§4.2 of docs/05-scheduling.md): that one started,
//! and that one ended, with its exit status and why. Each is believed only from a node the
//! workload is placed on, and only for a run of that workload that no other node started. How it
//! ended is the control plane's to judge, by the exit probe of the intent the run was made under,
//! never the node's to declare. The times are the node's clock, which is informational: each is
//! held between when the run could have happened and when the report arrived.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, RunWrite as _, Store};
use korpis_core::id::{Id, IdKind};
use korpis_core::node::journal::NodeEffect;
use korpis_core::workload::run::reported_end;
use serde_json::Value;
use std::time::SystemTime;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Applies a node's report of `workload`'s run, if `n` is one; why it was not believed, when
    /// it was not. A report the run has already moved past (a run cancelled before its start
    /// arrived, an end reported twice) is believed and changes nothing.
    pub(super) async fn run_reported(
        &self,
        tx: &mut S::Tx,
        (node, workload): (Uuid, Uuid),
        n: &NodeEffect,
        occurred: Option<SystemTime>,
    ) -> Result<Option<&'static str>, AppError> {
        let started = match n.action.as_str() {
            "run.started" => true,
            "run.ended" => false,
            _ => return Ok(None),
        };
        let field = |k: &str| n.after.as_ref().and_then(|a| a.get(k)).and_then(Value::as_str);
        let Some(id) = field("run").and_then(|s| Id::parse(IdKind::Run, s).ok()) else {
            return Ok(Some("the report names no run"));
        };
        let Some(run) = self.store.run(id.uuid()).await?.filter(|r| r.workload == workload) else {
            return Ok(Some("the run is not the workload's"));
        };
        if run.node.is_some_and(|by| by != node) {
            return Ok(Some("another node started the run"));
        }
        let now = self.clock.now();
        let floor = run.started_at.unwrap_or(run.created_at).min(now);
        let at = occurred.unwrap_or(now).min(now).max(floor);
        if started {
            tx.start_run(run.id, node, at).await?;
            return Ok(None);
        }
        let Some(intent) = self.store.intent(run.intent).await? else {
            return Ok(Some("the run's intent is gone"));
        };
        let exit_status = field("exit_status").and_then(|s| s.parse().ok());
        let end = reported_end(&intent.body, field("cause").unwrap_or("exit"), exit_status, at);
        tx.end_run(run.id, end).await?;
        Ok(None)
    }
}
