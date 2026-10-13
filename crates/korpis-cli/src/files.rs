// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis plan -f` and `korpis apply -f` over a set of files (§4 and §5 of
//! docs/13-surface-cli.md): each file declared, through a set when one is named; the set's
//! members the files no longer declare proposed for deletion; and, with `--plan`, exactly the
//! plans named applied, deletions among them.

use crate::apply::{Mode, Through, apply_planned, declare, plans};
use crate::connect::Connection;
use crate::declared::{File, read_all};
use crate::error::CliError;
use crate::sets::{SetArgs, confirm, orphans, preflight, propose_deletion, refusal};
use korpis_proto::korpis::v1 as pb;
use korpis_recipe::registry::Registry;
use std::path::PathBuf;

/// What a run of `plan` or `apply` did.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Each plan, with the name of the workload it is for.
    pub plans: Vec<(String, pb::Plan)>,
    /// The plans waiting for `apply --plan`.
    pub waiting: Vec<String>,
    /// Drifted members left as they are under `--accept-drift`.
    pub left: Vec<String>,
}

/// What a run is asked to do.
#[derive(Debug, Clone, Copy)]
pub struct Run<'a> {
    /// The files, or directories of them.
    pub paths: &'a [PathBuf],
    /// The plans `--plan` names; empty to declare.
    pub ids: &'a [String],
    /// `plan` rather than `apply`.
    pub propose: bool,
    /// The set flags.
    pub set: &'a SetArgs,
}

/// Runs `plan` or `apply`.
pub async fn run(c: &Connection, registry: &Registry, r: Run<'_>) -> Result<Outcome, CliError> {
    if !r.set.delete_volumes.is_empty() {
        return Err(CliError::Local(
            "--delete-volumes: deleting a volume arrives with volumes themselves in Phase 1 \
             (§4 of docs/20-roadmap.md); until then a deleted workload's volumes are kept"
                .into(),
        ));
    }
    let files = read_all(r.paths)?;
    let through = r.set.through();
    let mut out = Outcome::default();
    if let Some(t) = &through {
        let drifted = preflight(c, registry, &files, t).await?;
        if !drifted.is_empty() && !r.set.accept_drift {
            return Err(refusal(&drifted));
        }
        out.left = drifted.into_iter().map(|d| d.workload).collect();
    }
    let kept: Vec<&File> =
        files.iter().filter(|f| !out.left.contains(&f.declared.workload)).collect();
    let named = plans(c, r.ids).await?;
    for f in &kept {
        let plan = if r.ids.is_empty() {
            let mode = if r.propose { Mode::Propose } else { Mode::Apply };
            let d = declare(c, registry, f, mode, through.as_ref()).await?;
            d.plan.as_option().cloned().unwrap_or_default()
        } else {
            apply_planned(c, registry, f, &named, through.as_ref()).await?
        };
        if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
            out.waiting.push(plan.id.clone());
        }
        out.plans.push((f.declared.workload.clone(), plan));
    }
    if let Some(t) = &through {
        prune(c, &files, t, &r, &named, &mut out).await?;
    }
    Ok(out)
}

/// Deletes the set's members that no file declares: proposed by `plan`, proposed and confirmed
/// by `apply`, and with `--plan` confirmed only where a named plan is that deletion.
async fn prune(
    c: &Connection,
    files: &[File],
    through: &Through,
    r: &Run<'_>,
    named: &[pb::Plan],
    out: &mut Outcome,
) -> Result<(), CliError> {
    let gone = orphans(c, files, &through.name).await?;
    let retire = |p: &&pb::Plan| p.kind.as_known() == Some(pb::PlanKind::PLAN_KIND_RETIRE);
    for stray in named.iter().filter(retire) {
        if !gone.iter().any(|w| w.id == stray.workload) {
            return Err(CliError::Local(format!(
                "plan {} deletes a workload the files declare, or one outside the set",
                stray.id
            )));
        }
    }
    for w in gone {
        let plan = if r.ids.is_empty() {
            let proposed = propose_deletion(c, &w).await?;
            if r.propose { proposed } else { confirm(c, &proposed.id).await? }
        } else {
            match named.iter().filter(retire).find(|p| p.workload == w.id) {
                Some(p) => confirm(c, &p.id).await?,
                None => continue,
            }
        };
        if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
            out.waiting.push(plan.id.clone());
        }
        out.plans.push((w.name, plan));
    }
    Ok(())
}
