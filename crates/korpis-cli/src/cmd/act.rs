// SPDX-License-Identifier: AGPL-3.0-or-later

//! The imperative commands: one object, right now (§2 of docs/13-surface-cli.md). Each is one
//! call of the API after its names are resolved, and answers with what the API answered.

use super::{
    Answer, NodeAction, PlanAction, TokenAction, WorkloadAction, json, mutation, pairs, seconds,
};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{self, short};
use buffa_types::google::protobuf::Duration;
use korpis_proto::korpis::v1 as pb;

/// A plan the API answered, in both forms.
fn planned(plan: Option<&pb::Plan>) -> Answer {
    let p = plan.cloned().unwrap_or_default();
    Answer { human: show::plan(&p, &p.workload), json: json(&p) }
}

/// Answers `korpis plan approve|reject|cancel`.
pub async fn plan(c: &Connection, action: &PlanAction) -> Result<Answer, CliError> {
    let client = pb::PlanServiceClient::new(c.http(), c.config());
    let plan = match action {
        PlanAction::Approve { plan } => {
            let request = pb::ApprovePlanRequest {
                mutation: mutation(None).into(),
                plan: plan.clone(),
                ..Default::default()
            };
            client.approve_plan(request).await?.into_owned().plan
        }
        PlanAction::Reject { plan, reason } => {
            let request = pb::RejectPlanRequest {
                mutation: mutation(None).into(),
                plan: plan.clone(),
                reason: reason.clone(),
                ..Default::default()
            };
            client.reject_plan(request).await?.into_owned().plan
        }
        PlanAction::Cancel { plan } => {
            let request = pb::CancelPlanRequest {
                mutation: mutation(None).into(),
                plan: plan.clone(),
                ..Default::default()
            };
            client.cancel_plan(request).await?.into_owned().plan
        }
    };
    Ok(planned(plan.as_option()))
}

/// The power version of workload `id`, which a power change is made against.
async fn power_version(c: &Connection, id: &str) -> Result<u64, CliError> {
    let request = pb::GetWorkloadRequest { id: id.to_owned(), ..Default::default() };
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let status = client.get_workload(request).await?.into_owned().status;
    let workload = status.as_option().and_then(|s| s.workload.as_option());
    Ok(workload.and_then(|w| w.power.as_option()).map_or(0, |p| p.version))
}

/// What a power change answered: the declaration, and the plan carrying it when there is one.
fn powered(r: &pb::PowerResponse) -> Answer {
    let power = r.power.as_option().cloned().unwrap_or_default();
    let mut human = format!(
        "power: {} (version {}, cycle {})\n",
        short(power.desired, "POWER_STATE_"),
        power.version,
        power.cycle
    );
    if let Some(p) = r.plan.as_option() {
        human.push_str(&show::plan(p, &p.workload));
    }
    Answer { json: json(r), human }
}

/// Answers `korpis workload place`.
async fn place(
    c: &Connection,
    workload: String,
    node: &str,
    reason: &str,
) -> Result<Answer, CliError> {
    let request = pb::PlaceWorkloadRequest {
        mutation: mutation(None).into(),
        workload,
        node: resolve::node(c, node).await?,
        reason: reason.to_owned(),
        ..Default::default()
    };
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let r = client.place_workload(request).await?.into_owned();
    let p = r.placement.as_option().cloned().unwrap_or_default();
    let human = format!("placed on {} by an operator: {}\n", p.node, p.reason);
    Ok(Answer { json: json(&r), human })
}

/// Answers `korpis workload start|stop|restart|place|delete`.
pub async fn workload(c: &Connection, action: &WorkloadAction) -> Result<Answer, CliError> {
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let (WorkloadAction::Start { workload }
    | WorkloadAction::Stop { workload }
    | WorkloadAction::Restart { workload }
    | WorkloadAction::Place { workload, .. }
    | WorkloadAction::Delete { workload }) = action;
    let id = resolve::workload(c, workload).await?;
    if let WorkloadAction::Place { node, reason, .. } = action {
        return place(c, id, node, reason).await;
    }
    if matches!(action, WorkloadAction::Delete { .. }) {
        let request = pb::DeleteWorkloadRequest {
            mutation: mutation(None).into(),
            workload: id,
            ..Default::default()
        };
        let plan = client.delete_workload(request).await?.into_owned().plan;
        return Ok(planned(plan.as_option()));
    }
    let m = mutation(Some(power_version(c, &id).await?));
    let response = match action {
        WorkloadAction::Start { .. } => {
            let request =
                pb::StartRequest { mutation: m.into(), workload: id, ..Default::default() };
            client.start(request).await?
        }
        WorkloadAction::Stop { .. } => {
            let request =
                pb::StopRequest { mutation: m.into(), workload: id, ..Default::default() };
            client.stop(request).await?
        }
        WorkloadAction::Restart { .. }
        | WorkloadAction::Delete { .. }
        | WorkloadAction::Place { .. } => {
            let request =
                pb::RestartRequest { mutation: m.into(), workload: id, ..Default::default() };
            client.restart(request).await?
        }
    };
    Ok(powered(&response.into_owned()))
}

/// Answers `korpis node token create`: the token, once, and the command that redeems it.
async fn token(c: &Connection, server: &str, t: &TokenAction) -> Result<Answer, CliError> {
    let TokenAction::Create { name, organization, labels: given, expires } = t;
    let expires_in = match expires {
        Some(e) => Some(Duration { seconds: seconds(e)?, ..Default::default() }),
        None => None,
    };
    let request = pb::CreateEnrollmentTokenRequest {
        mutation: mutation(None).into(),
        node_name: name.clone(),
        labels: pairs("--label", given)?,
        organization: resolve::organization(c, organization.as_deref()).await?,
        expires_in: expires_in.into(),
        ..Default::default()
    };
    let client = pb::NodeServiceClient::new(c.http(), c.config());
    let r = client.create_enrollment_token(request).await?.into_owned();
    let human = format!(
        "token for {name}, redeemable once until {}; it is not shown again:\n\
         korpis-agent join --server {server} --token {} \\\n    --ca-fingerprint {}\n",
        show::time(r.expires_at.as_option()),
        r.token,
        r.ca_fingerprint
    );
    Ok(Answer { json: json(&r), human })
}

/// Answers `korpis node token create|cordon|uncordon`.
pub async fn node(c: &Connection, server: &str, action: &NodeAction) -> Result<Answer, CliError> {
    let client = pb::NodeServiceClient::new(c.http(), c.config());
    let node = match action {
        NodeAction::Token(t) => return token(c, server, t).await,
        NodeAction::Cordon { node } => {
            let node = resolve::node(c, node).await?;
            let request = pb::CordonNodeRequest {
                mutation: mutation(None).into(),
                node,
                ..Default::default()
            };
            client.cordon_node(request).await?.into_owned().node
        }
        NodeAction::Uncordon { node } => {
            let node = resolve::node(c, node).await?;
            let request = pb::UncordonNodeRequest {
                mutation: mutation(None).into(),
                node,
                ..Default::default()
            };
            client.uncordon_node(request).await?.into_owned().node
        }
    };
    let n = node.into_option().unwrap_or_default();
    let human = format!("{} ({})  {}\n", n.name, n.id, short(n.status, "NODE_STATUS_"));
    Ok(Answer { json: json(&n), human })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expiry_is_read_in_its_unit() {
        assert_eq!(seconds("90s").ok(), Some(90));
        assert_eq!(seconds("30m").ok(), Some(1800));
        assert_eq!(seconds("1h").ok(), Some(3600));
        assert_eq!(seconds("1d").ok(), Some(86_400));
        assert!(seconds("1").is_err());
        assert!(seconds("0h").is_err());
        assert!(seconds("h").is_err());
        assert!(pairs("--label", &["region=eu".into()]).is_ok_and(|l| l["region"] == "eu"));
        assert!(pairs("--label", &["region".into()]).is_err());
    }
}
