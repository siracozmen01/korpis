// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rows of the workload tables, and their conversions to the domain.

use crate::stored::{ImpactDoc, ScopeDoc, StepDoc, count, from_json, principal, to_time};
use crate::tenancy::{labels, name};
use chrono::{DateTime, Utc};
use korpis_app::ports::{IntentCodec, StoreError, StoreResult};
use korpis_core::workload::plan::{Approval, Override, PlanStatus, StepAction, VolumeOp};
use korpis_core::workload::{Intent, Plan, PlanTarget, Power, PowerRecord, PowerState, Workload};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

fn corrupt(what: &str, detail: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(format!("{what}: {detail}"))
}

pub(crate) struct WorkloadRow {
    pub(crate) id: Uuid,
    pub(crate) project_id: Uuid,
    pub(crate) organization_id: Uuid,
    pub(crate) name: String,
    pub(crate) labels: Value,
    pub(crate) intent_id: Option<Uuid>,
    pub(crate) intent_version: i64,
    pub(crate) target_id: Option<Uuid>,
    pub(crate) target_version: i64,
    pub(crate) power_version: i64,
    pub(crate) power_target_version: i64,
    pub(crate) version: i64,
    pub(crate) created: DateTime<Utc>,
    pub(crate) deleted_at: Option<DateTime<Utc>>,
    pub(crate) set_id: Option<Uuid>,
    pub(crate) classification: Value,
    pub(crate) unreproducible_at: Option<DateTime<Utc>>,
    pub(crate) unreproducible_reason: Option<String>,
}

impl WorkloadRow {
    pub(crate) fn into_domain(self) -> StoreResult<Workload> {
        Ok(Workload {
            id: self.id,
            project: self.project_id,
            organization: self.organization_id,
            name: name(self.name)?,
            labels: labels(self.labels)?,
            intent: self.intent_id.ok_or_else(|| corrupt("workload", "no head intent"))?,
            intent_version: count("intent_version", self.intent_version)?,
            target: self.target_id,
            target_version: count("target_version", self.target_version)?,
            power_version: count("power_version", self.power_version)?,
            power_target_version: count("power_target_version", self.power_target_version)?,
            version: count("workload version", self.version)?,
            created: to_time(self.created),
            deleted_at: self.deleted_at.map(to_time),
            set: self.set_id,
            classification: classification(self.classification)?,
            unreproducible: self.unreproducible_at.zip(self.unreproducible_reason).map(
                |(at, reason)| korpis_core::workload::Unreproducible { at: to_time(at), reason },
            ),
        })
    }
}

/// An operator's override as stored: an action name and whether it is disruptive.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct OverrideDoc {
    action: String,
    disruptive: bool,
}

/// Overrides to store.
pub(crate) fn classification_doc(overrides: &[Override]) -> Vec<OverrideDoc> {
    overrides
        .iter()
        .map(|o| OverrideDoc { action: o.action.as_str().to_owned(), disruptive: o.disruptive })
        .collect()
}

fn classification(v: Value) -> StoreResult<Vec<Override>> {
    let docs: Vec<OverrideDoc> = from_json("workload classification", v)?;
    docs.into_iter()
        .map(|d| {
            let action = StepAction::parse(&d.action)
                .ok_or_else(|| corrupt("classification action", &d.action))?;
            Ok(Override { action, disruptive: d.disruptive })
        })
        .collect()
}

pub(crate) struct IntentRow {
    pub(crate) id: Uuid,
    pub(crate) workload_id: Uuid,
    pub(crate) version: i64,
    pub(crate) parent_id: Option<Uuid>,
    pub(crate) declared_by: String,
    pub(crate) declared_at: DateTime<Utc>,
    pub(crate) body: Option<String>,
    pub(crate) schema_version: String,
    pub(crate) managed_by: Option<Uuid>,
}

impl IntentRow {
    pub(crate) fn into_domain(self, bodies: &dyn IntentCodec) -> StoreResult<Intent> {
        let json = self.body.ok_or_else(|| corrupt("intent", "no body"))?;
        Ok(Intent {
            id: self.id,
            workload: self.workload_id,
            version: count("intent version", self.version)?,
            parent: self.parent_id,
            declared_by: principal(&self.declared_by)?,
            declared_at: to_time(self.declared_at),
            body: bodies
                .decode(&json, &self.schema_version)
                .map_err(|e| corrupt("intent body", e))?,
            managed_by: self.managed_by,
        })
    }
}

pub(crate) fn desired_str(p: PowerState) -> &'static str {
    match p {
        PowerState::Running => "running",
        PowerState::Stopped => "stopped",
    }
}

pub(crate) struct PowerRow {
    pub(crate) workload_id: Uuid,
    pub(crate) version: i64,
    pub(crate) desired: String,
    pub(crate) cycle: i64,
    pub(crate) set_by: String,
    pub(crate) set_at: DateTime<Utc>,
}

impl PowerRow {
    pub(crate) fn into_domain(self) -> StoreResult<PowerRecord> {
        let desired = match self.desired.as_str() {
            "running" => PowerState::Running,
            "stopped" => PowerState::Stopped,
            other => return Err(corrupt("power desired", other)),
        };
        Ok(PowerRecord {
            workload: self.workload_id,
            power: Power {
                desired,
                cycle: count("cycle", self.cycle)?,
                version: count("power version", self.version)?,
            },
            set_by: principal(&self.set_by)?,
            set_at: to_time(self.set_at),
        })
    }
}

pub(crate) struct PlanRow {
    pub(crate) id: Uuid,
    pub(crate) workload_id: Uuid,
    pub(crate) kind: String,
    pub(crate) operation_id: Option<Uuid>,
    pub(crate) from_intent: Option<Uuid>,
    pub(crate) from_power: i64,
    pub(crate) to_intent: Option<Uuid>,
    pub(crate) to_power: Option<i64>,
    pub(crate) computed_at: DateTime<Utc>,
    pub(crate) computed_by: String,
    pub(crate) steps: Value,
    pub(crate) impact: Value,
    pub(crate) status: String,
    pub(crate) completed: Option<i32>,
    pub(crate) apply_after: Option<DateTime<Utc>>,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) approved_by: Option<String>,
    pub(crate) approved_at: Option<DateTime<Utc>>,
    pub(crate) proposed_under: Option<Uuid>,
    pub(crate) approval: String,
    pub(crate) approval_scope: Option<Value>,
    pub(crate) approval_reason: String,
    pub(crate) to_volume: Option<Uuid>,
    pub(crate) to_repository: Option<Uuid>,
}

/// What a plan's target columns hold: the intent, the power version, the volume, and the
/// repository it moves to or operates on.
type TargetColumns = (Option<Uuid>, Option<i64>, Option<Uuid>, Option<Uuid>);

fn target(
    kind: &str,
    (to_intent, to_power, to_volume, to_repository): TargetColumns,
) -> StoreResult<PlanTarget> {
    if let ("prune_backups", None, None, None, Some(r)) =
        (kind, to_intent, to_power, to_volume, to_repository)
    {
        return Ok(PlanTarget::Prune(r));
    }
    if to_repository.is_some() {
        return Err(corrupt("plan kind", kind));
    }
    Ok(match (kind, to_intent, to_power, to_volume) {
        ("intent", Some(i), None, None) => PlanTarget::Intent(i),
        ("power", None, Some(v), None) => PlanTarget::Power(count("to_power", v)?),
        ("retire", None, None, None) => PlanTarget::Retire,
        ("migrate", None, None, None) => PlanTarget::Migrate,
        (other, None, None, Some(v)) => match VolumeOp::parse(other) {
            Some(op) => PlanTarget::Volume(v, op),
            None => return Err(corrupt("plan kind", kind)),
        },
        _ => return Err(corrupt("plan kind", kind)),
    })
}

fn approval(kind: &str, scope: Option<Value>) -> StoreResult<Approval> {
    Ok(match (kind, scope) {
        ("auto", None) => Approval::Auto,
        ("confirmation", None) => Approval::Confirmation,
        ("authority", Some(s)) => Approval::Authority {
            scope: from_json::<ScopeDoc>("approval_scope", s)?.into_domain()?,
        },
        (other, _) => return Err(corrupt("plan approval", other)),
    })
}

impl PlanRow {
    pub(crate) fn into_domain(self) -> StoreResult<Plan> {
        let steps: Vec<StepDoc> = from_json("plan steps", self.steps)?;
        let impact: ImpactDoc = from_json("plan impact", self.impact)?;
        Ok(Plan {
            id: self.id,
            workload: self.workload_id,
            target: target(
                &self.kind,
                (self.to_intent, self.to_power, self.to_volume, self.to_repository),
            )?,
            operation: self.operation_id,
            from_intent: self.from_intent,
            from_power: count("from_power", self.from_power)?,
            computed_at: to_time(self.computed_at),
            computed_by: principal(&self.computed_by)?,
            proposed_under: self.proposed_under,
            steps: steps.into_iter().map(StepDoc::into_domain).collect::<Result<_, _>>()?,
            impact: impact.into_domain()?,
            approval: approval(&self.approval, self.approval_scope)?,
            approval_reason: self.approval_reason,
            status: PlanStatus::parse(&self.status)
                .ok_or_else(|| corrupt("plan status", &self.status))?,
            completed: self
                .completed
                .map(|c| u32::try_from(c).map_err(|_| corrupt("plan completed", c)))
                .transpose()?,
            apply_after: self.apply_after.map(to_time),
            expires_at: to_time(self.expires_at),
            approved_by: self.approved_by.as_deref().map(principal).transpose()?,
            approved_at: self.approved_at.map(to_time),
        })
    }
}

/// The approval columns of a plan.
pub(crate) fn approval_columns(a: &Approval) -> (&'static str, Option<ScopeDoc>) {
    match a {
        Approval::Auto => ("auto", None),
        Approval::Confirmation => ("confirmation", None),
        Approval::Authority { scope } => ("authority", Some(ScopeDoc::from_domain(scope))),
    }
}
