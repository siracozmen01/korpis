// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declaring an intent: a new workload, or a new intent for an existing one (§3.3 of
//! docs/01-model.md). Declaring moves the head; only approving moves the target, and the plan is
//! approved in the same call only when nothing in it needs a human (§3.2 of docs/03-state.md).

mod commit;
mod set;

use super::propose::{self, Proposal};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, WorkloadWrite};
use commit::{approval_effect, declaration_effects, moved};
use korpis_core::authz::{Kind, ObjectRef, Permit};
use korpis_core::labels::Labels;
use korpis_core::recipe::Recipe;
use korpis_core::recipe::resolve::Change;
use korpis_core::tenancy::Name;
use korpis_core::tenancy::quota::Held;
use korpis_core::workload::declare::Declaration;
use korpis_core::workload::plan::compute::intent_steps;
use korpis_core::workload::plan::impact::delta;
use korpis_core::workload::plan::{Approval, DriverClass, Proposing};
use korpis_core::workload::set::Through;
use korpis_core::workload::spec::IntentBody;
use korpis_core::workload::{Intent, Plan, PlanTarget, Power, PowerRecord, PowerState, Workload};
use uuid::Uuid;

const OPERATION: &str = "workload.declare_intent";

/// A workload to create with the intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWorkload {
    /// The project it belongs to.
    pub project: Uuid,
    /// Its name, unique within the project.
    pub name: Name,
    /// For selectors.
    pub labels: Labels,
    /// Its first power declaration.
    pub power: PowerState,
}

/// What the intent is declared for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclareTarget {
    /// A new workload.
    New(NewWorkload),
    /// An existing workload, whose head the caller saw at `expected_version`.
    Existing {
        /// The workload.
        workload: Uuid,
        /// The head's version the declaration was made against (§6 of docs/10-api.md).
        expected_version: u64,
    },
}

/// A declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclareIntent {
    /// For what.
    pub target: DeclareTarget,
    /// The recipe's repository.
    pub recipe_name: String,
    /// A tag, or a digest.
    pub recipe_version: String,
    /// What is declared; the recipe completes the rest (§3.1 of docs/09-recipes.md).
    pub declaration: Declaration,
    /// Compute and return everything, persist nothing.
    pub dry_run: bool,
    /// Persist everything and approve nothing: the plan waits for its proposer to confirm it
    /// even when default policy would approve it (§3.3 of docs/01-model.md).
    pub propose: bool,
    /// Through a declared set (§3.3 of docs/01-model.md).
    pub set: Option<SetDeclaration>,
}

/// A declaration through a declared set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetDeclaration {
    /// The set's name in the workload's project; the declaration creates it if it is new.
    pub name: Name,
    /// Onto a workload that belongs to no set: take it in.
    pub adopt: bool,
    /// Onto a member drifted in advisory mode: discard the out-of-band change.
    pub reconcile: bool,
}

/// What a declaration produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Declared {
    /// The workload, with its head on the new intent.
    pub workload: Workload,
    /// The new intent.
    pub intent: Intent,
    /// The current power declaration.
    pub power: PowerRecord,
    /// The plan toward the intent.
    pub plan: Plan,
}

/// The state a declaration is made against.
struct Base {
    workload: Workload,
    power: PowerRecord,
    head: Option<Intent>,
    target: Option<Intent>,
    permit: Permit,
    proposing: Proposing,
    action: &'static str,
}

impl Base {
    /// What the workload holds now: the larger of its head and its target; nothing if new.
    fn held(&self) -> Option<Held> {
        let head = Held::of(&self.head.as_ref()?.body);
        Some(match &self.target {
            Some(t) => head.max(&Held::of(&t.body)),
            None => head,
        })
    }

    /// What it will hold with `intent` as its head, beside the target it still runs.
    fn holding(&self, intent: &Intent) -> Held {
        let head = Held::of(&intent.body);
        match &self.target {
            Some(t) => head.max(&Held::of(&t.body)),
            None => head,
        }
    }
}

/// What declaring `intent` with `plan` against `base` produces.
fn declared(base: &Base, intent: Intent, plan: Plan, passage: Option<&set::Passage>) -> Declared {
    // Created through a set, or adopted into one, the workload is its member from now on.
    let joins = passage.filter(|p| matches!(p.through, None | Some(Through::Adopted)));
    Declared {
        workload: Workload {
            intent: intent.id,
            intent_version: intent.version,
            set: joins.map(set::Passage::set_id).or(base.workload.set),
            ..base.workload.clone()
        },
        intent,
        power: base.power.clone(),
        plan,
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Declares an intent. A grant that may only propose produces a plan that waits for an
    /// approver; a plan nothing in which needs a human is approved at once and moves the target.
    pub async fn declare_intent(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Declared>,
        cmd: DeclareIntent,
    ) -> Result<Reply<Declared>, AppError> {
        if cmd.dry_run && cmd.propose {
            return Err(AppError::invalid("propose", "a dry run persists nothing to propose"));
        }
        if !cmd.dry_run
            && let Some(r) = self.replay(caller, idem, OPERATION).await?
        {
            return Ok(Reply::Replayed(r));
        }
        let base = match &cmd.target {
            DeclareTarget::New(n) => self.base_new(caller, n).await?,
            DeclareTarget::Existing { workload, expected_version } => {
                self.base_existing(caller, *workload, *expected_version).await?
            }
        };
        let recipe_ref = (cmd.recipe_name.as_str(), cmd.recipe_version.as_str());
        let (body, recipe, changes) =
            self.complete_declaration(caller, &base, recipe_ref, cmd.declaration).await?;
        let passage = match &cmd.set {
            Some(through) => Some(self.passage(caller, &base, through, &body).await?),
            None => None,
        };
        let intent = self.intent_of(caller, &base, body, passage.as_ref());
        let plan = self.intent_plan(caller, &base, &intent, &recipe, cmd.propose);
        let mut declared = declared(&base, intent, plan, passage.as_ref());
        let held = base.held();
        let holds = base.holding(&declared.intent);
        let mut tx = self.store.begin().await?;
        if cmd.dry_run {
            // Admission runs, so the plan shows a quota refusal too, and the transaction is
            // dropped: nothing is held and nothing is written.
            self.admit(&mut tx, &declared.workload, held.as_ref(), &holds).await?;
            return Ok(Reply::Fresh(declared));
        }
        if let Some(p) = &passage {
            self.create_set(&mut tx, p).await?;
        }
        self.write_declaration(&mut tx, &base, &declared, held.as_ref(), &holds).await?;
        if let Some(p) = &passage {
            self.record_passage(&mut tx, caller, &base, &declared, p).await?;
        }
        let superseded = tx.supersede_proposed(&declared.plan).await?;
        let approved = self.approve_declared(&mut tx, caller, &base, &mut declared).await?;
        for effect in declaration_effects(caller, &base, &declared, &changes, superseded) {
            tx.append_effect(&effect).await?;
        }
        if approved {
            tx.append_effect(&approval_effect(caller, &base, &declared.plan)).await?;
            self.place_target(&mut tx, &declared.workload, Some(&declared.intent)).await?;
        }
        finish(tx, caller, idem, OPERATION, declared).await
    }

    /// The intent `body` declares against `base`, through `passage` when the declaration goes
    /// through a set.
    fn intent_of(
        &self,
        caller: &Caller,
        base: &Base,
        body: IntentBody,
        passage: Option<&set::Passage>,
    ) -> Intent {
        Intent {
            id: Uuid::now_v7(),
            workload: base.workload.id,
            version: base.head.as_ref().map_or(1, |h| h.version + 1),
            parent: base.head.as_ref().map(|h| h.id),
            declared_by: caller.principal.clone(),
            declared_at: self.clock.now(),
            body,
            managed_by: passage.map(set::Passage::set_id),
        }
    }

    /// The declared body completed from its recipe, resolved under the organization's trust
    /// policy, with its configuration resolved and what that changed.
    async fn complete_declaration(
        &self,
        caller: &Caller,
        base: &Base,
        (name, version): (&str, &str),
        declaration: Declaration,
    ) -> Result<(IntentBody, Recipe, Vec<Change>), AppError> {
        let organization = base.workload.organization;
        let (reference, recipe) = self.resolve_trusted(name, version, organization).await?;
        let mut body = declaration
            .complete(reference, &recipe)
            .map_err(|e| AppError::invalid(format!("body.{}", e.field), e.message))?;
        if !body.dependencies.is_empty() {
            // The schema is frozen here (rule E); resolving a dependency and injecting its
            // address arrive with service discovery (§4 of docs/20-roadmap.md). Accepting one now
            // would store a promise nothing keeps.
            return Err(AppError::NotYet { what: "dependencies", phase: 1 });
        }
        let previous = base.head.as_ref().map(|h| &h.body.config);
        let changes = self.configure(caller, &mut body, &recipe, previous, organization).await?;
        Ok((body, recipe, changes))
    }

    async fn base_new(&self, caller: &Caller, n: &NewWorkload) -> Result<Base, AppError> {
        let object = ObjectRef::new(Kind::Project, n.project);
        let (permit, proposing) =
            self.authorize_declaration(caller, "workload.create", object).await?;
        let project =
            self.store.project(n.project).await?.ok_or(AppError::NotFound { kind: "project" })?;
        let now = self.clock.now();
        let id = Uuid::now_v7();
        let workload = Workload {
            id,
            project: project.id,
            organization: project.organization,
            name: n.name.clone(),
            labels: n.labels.clone(),
            intent: Uuid::nil(),
            intent_version: 0,
            target: None,
            target_version: 0,
            power_version: 1,
            power_target_version: 0,
            version: 1,
            created: now,
            deleted_at: None,
            set: None,
        };
        let power = PowerRecord {
            workload: id,
            power: Power::initial(n.power),
            set_by: caller.principal.clone(),
            set_at: now,
        };
        let action = "workload.create";
        Ok(Base { workload, power, head: None, target: None, permit, proposing, action })
    }

    async fn base_existing(
        &self,
        caller: &Caller,
        id: Uuid,
        expected: u64,
    ) -> Result<Base, AppError> {
        let object = ObjectRef::new(Kind::Workload, id);
        let (permit, proposing) =
            self.authorize_declaration(caller, "workload.write", object).await?;
        let workload = self
            .store
            .workload(id)
            .await?
            .filter(|w| w.deleted_at.is_none())
            .ok_or(AppError::NotFound { kind: "workload" })?;
        if workload.intent_version != expected {
            return Err(moved(&workload));
        }
        let intent = |i: Uuid| async move {
            self.store.intent(i).await?.ok_or_else(|| AppError::Internal(format!("intent {i}")))
        };
        let head = Some(intent(workload.intent).await?);
        let target = match workload.target {
            Some(t) => Some(intent(t).await?),
            None => None,
        };
        let power = self
            .store
            .power(id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("workload {id} has no power")))?;
        let action = "workload.write";
        Ok(Base { workload, power, head, target, permit, proposing, action })
    }

    /// The plan of `to`; with `propose`, one that waits for confirmation whatever default policy
    /// would do with it (§3.3 of docs/01-model.md).
    fn intent_plan(
        &self,
        caller: &Caller,
        base: &Base,
        to: &Intent,
        recipe: &Recipe,
        propose: bool,
    ) -> Plan {
        let driver = DriverClass::default();
        let classifier = propose::classifier(&driver, &recipe.disruption);
        let from = base.target.as_ref().map(|t| (t.id, &t.body));
        let steps = intent_steps(
            to.workload,
            from.map(|(_, b)| b),
            &to.body,
            base.power.power.desired,
            classifier,
        );
        let proposal = Proposal {
            workload: to.workload,
            target: PlanTarget::Intent(to.id),
            from_intent: from.map(|(id, _)| id),
            from_power: base.workload.power_target_version,
            delta: delta(from.map(|(_, b)| b), Some(&to.body)),
            steps,
            proposing: &base.proposing,
            grant: base.permit.grant,
        };
        let mut plan = propose::plan(caller, proposal, self.clock.now());
        if propose && plan.approval == Approval::Auto {
            plan.approval = Approval::Confirmation;
            plan.approval_reason = "its proposer asked to confirm it".into();
        }
        plan
    }
}
