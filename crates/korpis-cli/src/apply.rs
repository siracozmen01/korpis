// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis plan -f` and `korpis apply -f` (§4 of docs/13-surface-cli.md). Each file is checked
//! locally and then declared. `plan` declares with `propose`, so the answer is a real, persisted
//! plan that waits to be confirmed. `apply` declares and lets default policy approve what it may;
//! with `--plan` it applies exactly the plans named, after checking that the files still declare
//! what each planned, and answers `CONFLICT` with the plan they declare now when they do not.

use crate::check::check;
use crate::connect::Connection;
use crate::declared::File;
use crate::error::CliError;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_recipe::registry::Registry;

/// How a file is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Persist, and wait for confirmation whatever policy would do.
    Propose,
    /// Persist, and let default policy approve what it may.
    Apply,
    /// Compute, persist nothing.
    DryRun,
}

fn conflict(message: String) -> CliError {
    CliError::Api {
        code: pb::ErrorCode::ERROR_CODE_CONFLICT,
        reason: "plan.changed".into(),
        field: String::new(),
        message,
    }
}

/// A declaration through a declared set (§5 and §6 of docs/13-surface-cli.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Through {
    /// The set's name in each file's project.
    pub name: String,
    /// Take workloads that belong to no set into it.
    pub adopt: bool,
    /// Discard out-of-band changes to members of an advisory set.
    pub reconcile: bool,
}

/// The workload `name` in `project`, as the API renders it, if it exists.
pub async fn existing(
    c: &Connection,
    project: &str,
    name: &str,
) -> Result<Option<pb::WorkloadStatus>, CliError> {
    let request =
        pb::ListWorkloadsRequest { project: Some(project.to_owned()), ..Default::default() };
    let listed = pb::WorkloadServiceClient::new(c.http(), c.config())
        .list_workloads(request)
        .await?
        .into_owned()
        .workloads;
    Ok(listed.into_iter().find(|s| s.workload.as_option().is_some_and(|w| w.name == name)))
}

/// Declares `file` in `mode`, checked first against its recipe, through `through` when it names
/// a set.
pub async fn declare(
    c: &Connection,
    registry: &Registry,
    file: &File,
    mode: Mode,
    through: Option<&Through>,
) -> Result<pb::DeclareIntentResponse, CliError> {
    // The request and its recipe check make a large future; it is boxed here, once, rather than
    // at every caller.
    Box::pin(declared(c, registry, file, mode, through)).await
}

async fn declared(
    c: &Connection,
    registry: &Registry,
    file: &File,
    mode: Mode,
    through: Option<&Through>,
) -> Result<pb::DeclareIntentResponse, CliError> {
    let d = &file.declared;
    let body = d.body().map_err(|e| CliError::Local(format!("{}: {e}", file.path.display())))?;
    check(file, &body, registry).await?;
    let project = crate::resolve::project(c, &d.project).await?;
    let mut mutation =
        pb::Mutation { idempotency_key: uuid::Uuid::now_v7().to_string(), ..Default::default() };
    let found = existing(c, &project, &d.workload).await?;
    let found = found.as_ref().and_then(|s| Some((s.workload.as_option()?, s.head.as_option()?)));
    let target = match found {
        Some((w, head)) => {
            mutation.expected_version = Some(head.version);
            Target::Workload(w.id.clone())
        }
        None => Target::Create(Box::new(pb::NewWorkload {
            project,
            name: d.workload.clone(),
            labels: d.labels.clone(),
            power: match d.power.as_deref() {
                Some("stopped") => pb::PowerState::POWER_STATE_STOPPED.into(),
                _ => pb::PowerState::POWER_STATE_RUNNING.into(),
            },
            ..Default::default()
        })),
    };
    let request = pb::DeclareIntentRequest {
        mutation: mutation.into(),
        target: Some(target),
        body: body.into(),
        dry_run: mode == Mode::DryRun,
        propose: mode == Mode::Propose,
        set: through.map(|t| t.name.clone()).unwrap_or_default(),
        adopt: through.is_some_and(|t| t.adopt),
        reconcile: through.is_some_and(|t| t.reconcile),
        ..Default::default()
    };
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    Ok(client.declare_intent(request).await?.into_owned())
}

/// The intent `id` of `workload`.
async fn intent(c: &Connection, workload: &str, id: &str) -> Result<pb::Intent, CliError> {
    let request = pb::ListIntentsRequest { workload: workload.to_owned(), ..Default::default() };
    let intents = pb::WorkloadServiceClient::new(c.http(), c.config())
        .list_intents(request)
        .await?
        .into_owned()
        .intents;
    intents.into_iter().find(|i| i.id == id).ok_or_else(|| conflict(format!("intent {id} is gone")))
}

/// Applies the plan among `plans` that is for `file`'s workload, if `file` still declares what
/// it planned; the approved plan.
pub async fn apply_planned(
    c: &Connection,
    registry: &Registry,
    file: &File,
    plans: &[pb::Plan],
    through: Option<&Through>,
) -> Result<pb::Plan, CliError> {
    let now = declare(c, registry, file, Mode::DryRun, through).await?;
    let workload = now.workload.as_option().map(|w| w.id.clone()).unwrap_or_default();
    let plan = plans.iter().find(|p| p.workload == workload).ok_or_else(|| {
        CliError::Local(format!(
            "no --plan is for {:?} ({})",
            file.declared.workload,
            file.path.display()
        ))
    })?;
    let planned = intent(c, &workload, plan.to_intent.as_deref().unwrap_or_default()).await?;
    let declared_now = now.intent.as_option().and_then(|i| i.body.as_option());
    if planned.body.as_option() != declared_now {
        let new = now
            .plan
            .as_option()
            .map_or_else(String::new, |p| crate::show::plan(p, &file.declared.workload));
        return Err(conflict(format!(
            "{} no longer declares what plan {} planned; it now plans:\n{new}",
            file.path.display(),
            plan.id
        )));
    }
    let request = pb::ApprovePlanRequest {
        mutation: pb::Mutation {
            idempotency_key: uuid::Uuid::now_v7().to_string(),
            ..Default::default()
        }
        .into(),
        plan: plan.id.clone(),
        ..Default::default()
    };
    let approved = pb::PlanServiceClient::new(c.http(), c.config()).approve_plan(request).await?;
    Ok(approved.into_owned().plan.into_option().unwrap_or_default())
}

/// The plans `ids` names.
pub async fn plans(c: &Connection, ids: &[String]) -> Result<Vec<pb::Plan>, CliError> {
    let client = pb::PlanServiceClient::new(c.http(), c.config());
    let mut out = Vec::new();
    for id in ids {
        let request = pb::GetPlanRequest { id: id.clone(), ..Default::default() };
        out.extend(client.get_plan(request).await?.into_owned().plan.into_option());
    }
    Ok(out)
}
