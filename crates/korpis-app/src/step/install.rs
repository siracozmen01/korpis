// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resolving a provider step of an install a node runs (§4 of docs/09-recipes.md). The node asks
//! for one action of an install step; the control plane checks that the node is where the
//! workload is placed and the plan is still open, that the installation the intent pinned still
//! answers for the step kind (§4.1 of docs/16-extensions.md), calls it, and marks the workload
//! unreproducible the first time a step resolves to whatever a channel held (§3.3 of
//! docs/01-model.md).

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{Clock, RecipeSource, Store, Tx as _, WorkloadWrite as _};
use crate::step::StepCall;
use korpis_core::extension::{CallOutcome, Platform, ProviderType, StepResult};
use korpis_core::recipe::ProviderPin;
use korpis_core::recipe::install::InstallStep;
use korpis_core::recipe::installing::actions_of;
use korpis_core::workload::plan::{PlanTarget, StepAction};
use std::time::Duration;
use uuid::Uuid;

/// How long one resolution may wait for its provider.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(120);

/// The action of a plan's install step a node asks to be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallAt {
    /// The plan.
    pub plan: Uuid,
    /// The install step, by index.
    pub step: u32,
    /// The provider step, by its index among the step's actions.
    pub action: u32,
}

/// Why a provider step was not resolved for a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepRefusal {
    /// The plan is not open, or the workload is not placed on the node.
    NotAssigned,
    /// The plan has no such install step, or the action is not a provider step.
    NotAStep,
    /// The installation the intent pinned no longer answers for the step kind: it was upgraded,
    /// uninstalled, or another now answers. Declaring the workload again pins what answers now.
    ProviderChanged,
    /// The provider's own failure, or its breaker's.
    Failed {
        /// Its stable reason.
        reason: String,
        /// Whether asking again may succeed.
        retryable: bool,
    },
}

impl StepRefusal {
    /// The stable reason the node is told.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::NotAssigned => "step.not_assigned".into(),
            Self::NotAStep => "step.not_a_step".into(),
            Self::ProviderChanged => "step.provider_changed".into(),
            Self::Failed { reason, .. } => reason.clone(),
        }
    }

    /// Whether asking again, unchanged, may succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Failed { retryable, .. } => *retryable,
            _ => false,
        }
    }
}

type Resolved = Result<Result<StepResult, StepRefusal>, AppError>;

/// The provider step `at` names in `plan`: its kind and arguments.
fn provider_step(
    plan: &korpis_core::workload::Plan,
    at: InstallAt,
) -> Result<Option<(String, korpis_core::value::ValueMap)>, AppError> {
    let Some(s) = plan.steps.iter().find(|s| s.index == at.step && s.action == StepAction::Install)
    else {
        return Ok(None);
    };
    let actions = actions_of(&s.detail)
        .map_err(|e| AppError::Internal(format!("plan {} step {}: {e}", plan.id, s.index)))?;
    let action = usize::try_from(at.action).ok().and_then(|i| actions.into_iter().nth(i));
    Ok(match action.map(|a| a.step) {
        Some(InstallStep::Provider { provider, args, .. }) => Some((provider, args)),
        _ => None,
    })
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Resolves the provider step `at` for `node`, on `platform`.
    pub async fn resolve_install_step(
        &self,
        node: Uuid,
        at: InstallAt,
        platform: Platform,
    ) -> Resolved {
        let Some(p) = self.store.plan(at.plan).await?.filter(|p| !p.status.is_terminal()) else {
            return Ok(Err(StepRefusal::NotAssigned));
        };
        if self.store.placement(p.workload).await?.is_none_or(|x| x.node != node) {
            return Ok(Err(StepRefusal::NotAssigned));
        }
        let (Some((kind, arguments)), PlanTarget::Intent(intent)) =
            (provider_step(&p, at)?, p.target)
        else {
            return Ok(Err(StepRefusal::NotAStep));
        };
        let missing = |what: &str| AppError::Internal(format!("plan {}: {what}", p.id));
        let intent = self.store.intent(intent).await?.ok_or_else(|| missing("no intent"))?;
        let workload =
            self.store.workload(p.workload).await?.ok_or_else(|| missing("no workload"))?;
        let pin = intent
            .body
            .recipe
            .providers
            .iter()
            .find(|x| x.kind == ProviderType::Step && x.name == kind);
        let target = CallTarget {
            organization: workload.organization,
            kind: ProviderType::Step,
            name: kind,
        };
        if !self.still_answers(&target, pin).await? {
            return Ok(Err(StepRefusal::ProviderChanged));
        }
        let call = StepCall { workload: workload.id, arguments, platform };
        match self.run_step(&target, call, STEP_TIMEOUT).await?.outcome {
            CallOutcome::StepResult(r) => {
                if !r.pinned {
                    let reason = format!("{} resolved to {}", target.name, r.resolved);
                    self.mark_unreproducible(workload.id, &reason).await?;
                }
                Ok(Ok(r))
            }
            CallOutcome::Failed(f) => {
                Ok(Err(StepRefusal::Failed { reason: f.reason, retryable: f.retryable }))
            }
            _ => Ok(Err(StepRefusal::Failed {
                reason: "provider.malformed".into(),
                retryable: true,
            })),
        }
    }

    /// Whether the installation `pin` names is the one that answers `target` now, at the digest
    /// it was pinned at.
    pub(crate) async fn still_answers(
        &self,
        target: &CallTarget,
        pin: Option<&ProviderPin>,
    ) -> Result<bool, AppError> {
        let Some(pin) = pin else { return Ok(false) };
        let Some(owner) = self.provider_owner(target).await? else { return Ok(false) };
        let now = self.store.providing_installation(owner, target.kind, &target.name).await?;
        Ok(now.is_some_and(|i| i.id == pin.extension && i.artifact_digest == pin.digest))
    }

    /// Marks `workload` unreproducible for `reason`, unless it already is.
    async fn mark_unreproducible(&self, workload: Uuid, reason: &str) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        // Whether this call or an earlier one set it, the workload is marked; the first reason
        // stands (§3.3 of docs/01-model.md).
        tx.mark_unreproducible(workload, self.clock.now(), reason).await?;
        tx.commit().await?;
        Ok(())
    }
}
