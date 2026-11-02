// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis operation …`: draining or evacuating a node, or rolling a template version out to the
//! grants issued from it, as one decision a person proposes, reads, and approves.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use clap::{Args, Subcommand};
use korpis_extension::Transport;
use korpis_proto::korpis::v1 as pb;

/// How an operation proceeds, as typed.
#[derive(Debug, Clone, Args)]
pub struct PolicyArgs {
    /// The most items applying at once.
    #[arg(long, default_value_t = 1)]
    concurrency: u32,
    /// The most workloads applying, or moved and not yet seen running, at once.
    #[arg(long, default_value_t = 1)]
    max_unavailable: u32,
    /// What a failed item does: `pause` (the default), `continue`, or `abort`.
    #[arg(long, default_value = "pause")]
    on_failure: String,
}

/// `korpis operation …`.
#[derive(Debug, Clone, Subcommand)]
pub enum OperationAction {
    /// Proposes moving every workload off a node that takes part, each by a migration.
    Drain {
        /// The node, by name or identifier.
        node: String,
        /// How it proceeds.
        #[command(flatten)]
        policy: PolicyArgs,
    },
    /// Proposes placing every workload of an unreachable node again elsewhere; each item says
    /// where its data comes from and what is lost.
    Evacuate {
        /// The node, by name or identifier.
        node: String,
        /// How it proceeds.
        #[command(flatten)]
        policy: PolicyArgs,
    },
    /// Proposes giving every grant issued from a template at an earlier version what `version`
    /// adds.
    Rollout {
        /// The template (`tpl_…`).
        template: String,
        /// The version whose additions it gives.
        #[arg(long)]
        version: u64,
        /// How it proceeds.
        #[command(flatten)]
        policy: PolicyArgs,
    },
    /// Approves one, which begins at once.
    Approve {
        /// The operation (`op_…`).
        operation: String,
        /// For a rollout: the grant you hold that its grants are attenuated from (`grt_…`).
        #[arg(long)]
        parent: Option<String>,
    },
    /// Begins nothing more until resumed.
    Pause {
        /// The operation (`op_…`).
        operation: String,
    },
    /// Begins again.
    Resume {
        /// The operation (`op_…`).
        operation: String,
    },
    /// Ends it: what applies goes on, and the rest is skipped.
    Abort {
        /// The operation (`op_…`).
        operation: String,
        /// Why, for the record.
        #[arg(long)]
        reason: String,
    },
    /// The operations, newest first, or one of them with its items.
    Get {
        /// One operation (`op_…`).
        operation: Option<String>,
        /// Only those over this node.
        #[arg(long)]
        node: Option<String>,
    },
}

fn policy(p: &PolicyArgs) -> Result<pb::OperationPolicy, CliError> {
    let on_failure = match p.on_failure.as_str() {
        "pause" => pb::OnFailure::ON_FAILURE_PAUSE,
        "continue" => pb::OnFailure::ON_FAILURE_CONTINUE,
        "abort" => pb::OnFailure::ON_FAILURE_ABORT,
        other => {
            return Err(CliError::Local(format!(
                "--on-failure {other}: pause, continue, or abort"
            )));
        }
    };
    Ok(pb::OperationPolicy {
        concurrency: p.concurrency,
        max_unavailable: p.max_unavailable,
        on_failure: on_failure.into(),
        ..Default::default()
    })
}

/// One operation, with each of its items.
fn one(op: &pb::Operation) -> String {
    let over = op.node.clone().or_else(|| op.template.clone()).unwrap_or_default();
    let p = op.progress.as_option().cloned().unwrap_or_default();
    let mut out = format!(
        "operation {} {} of {}: {}{}\n{} of {} done, {} applying, {} settling, {} failed, {} skipped\n",
        op.id,
        short(op.kind, "OPERATION_KIND_"),
        over,
        short(op.status, "OPERATION_STATUS_"),
        if op.reason.is_empty() { String::new() } else { format!(": {}", op.reason) },
        p.done,
        p.total,
        p.applying,
        p.settling,
        p.failed,
        p.skipped,
    );
    let mut rows = vec![["ITEM", "STATE", "PLAN", "DETAIL", "REASON"].map(str::to_owned).to_vec()];
    for i in &op.items {
        rows.push(vec![
            i.subject.clone(),
            short(i.state, "OPERATION_ITEM_STATE_"),
            i.plan.clone().or_else(|| i.issued.clone()).unwrap_or_else(|| "-".into()),
            json(&i.detail).to_string(),
            i.reason.clone(),
        ]);
    }
    out.push_str(&table(&rows));
    out
}

/// Operations, one line each.
fn rows(list: &[pb::Operation]) -> String {
    let mut rows =
        vec![["OPERATION", "KIND", "STATUS", "PROPOSED", "DONE"].map(str::to_owned).to_vec()];
    for op in list {
        let p = op.progress.as_option().cloned().unwrap_or_default();
        rows.push(vec![
            op.id.clone(),
            short(op.kind, "OPERATION_KIND_"),
            short(op.status, "OPERATION_STATUS_"),
            time(op.proposed_at.as_option()),
            format!("{}/{}", p.done, p.total),
        ]);
    }
    table(&rows)
}

/// Proposes an operation of `kind`, completed by `with`.
async fn propose(
    client: &pb::OperationServiceClient<Transport>,
    kind: pb::OperationKind,
    p: &PolicyArgs,
    with: impl FnOnce(&mut pb::ProposeOperationRequest),
) -> Result<Option<pb::Operation>, CliError> {
    let mut r = pb::ProposeOperationRequest {
        mutation: mutation(None).into(),
        kind: kind.into(),
        policy: policy(p)?.into(),
        ..Default::default()
    };
    with(&mut r);
    Ok(client.propose_operation(r).await?.into_owned().operation.into_option())
}

/// Answers `korpis operation …`.
pub async fn operation(c: &Connection, action: &OperationAction) -> Result<Answer, CliError> {
    use pb::OperationKind as K;
    let client = pb::OperationServiceClient::new(c.http(), c.config());
    let op = match action {
        OperationAction::Drain { node, policy } => {
            let node = resolve::node(c, node).await?;
            propose(&client, K::OPERATION_KIND_DRAIN, policy, |r| r.node = node).await?
        }
        OperationAction::Evacuate { node, policy } => {
            let node = resolve::node(c, node).await?;
            propose(&client, K::OPERATION_KIND_EVACUATE, policy, |r| r.node = node).await?
        }
        OperationAction::Rollout { template, version, policy } => {
            let rollout = |r: &mut pb::ProposeOperationRequest| {
                r.template.clone_from(template);
                r.template_version = *version;
            };
            propose(&client, K::OPERATION_KIND_TEMPLATE_ROLLOUT, policy, rollout).await?
        }
        OperationAction::Get { operation: None, node } => {
            let node = match node {
                Some(n) => resolve::node(c, n).await?,
                None => String::new(),
            };
            let r = pb::ListOperationsRequest { node, ..Default::default() };
            let list = client.list_operations(r).await?.into_owned().operations;
            return Ok(Answer { human: rows(&list), json: json(&list) });
        }
        other => decide(&client, other).await?,
    };
    let op = op.unwrap_or_default();
    Ok(Answer { human: one(&op), json: json(&op) })
}

/// Answers `korpis operation approve|pause|resume|abort|get <operation>`.
async fn decide(
    client: &pb::OperationServiceClient<Transport>,
    action: &OperationAction,
) -> Result<Option<pb::Operation>, CliError> {
    let m = || mutation(None).into();
    Ok(match action {
        OperationAction::Approve { operation, parent } => {
            let r = pb::ApproveOperationRequest {
                mutation: m(),
                operation: operation.clone(),
                parent: parent.clone().unwrap_or_default(),
                ..Default::default()
            };
            client.approve_operation(r).await?.into_owned().operation.into_option()
        }
        OperationAction::Pause { operation } => {
            let r = pb::PauseOperationRequest {
                mutation: m(),
                operation: operation.clone(),
                ..Default::default()
            };
            client.pause_operation(r).await?.into_owned().operation.into_option()
        }
        OperationAction::Resume { operation } => {
            let r = pb::ResumeOperationRequest {
                mutation: m(),
                operation: operation.clone(),
                ..Default::default()
            };
            client.resume_operation(r).await?.into_owned().operation.into_option()
        }
        OperationAction::Abort { operation, reason } => {
            let r = pb::AbortOperationRequest {
                mutation: m(),
                operation: operation.clone(),
                reason: reason.clone(),
                ..Default::default()
            };
            client.abort_operation(r).await?.into_owned().operation.into_option()
        }
        OperationAction::Get { operation: Some(id), .. } => {
            let r = pb::GetOperationRequest { id: id.clone(), ..Default::default() };
            client.get_operation(r).await?.into_owned().operation.into_option()
        }
        _ => None,
    })
}
