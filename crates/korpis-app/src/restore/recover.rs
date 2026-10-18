// SPDX-License-Identifier: AGPL-3.0-or-later

//! The declarations a restore recovers from what nodes present (§8 of docs/03-state.md): a
//! workload the store lacks, under its signed owner; its target intent with the identifier,
//! version, body, and declarer it was signed with; its power declaration; and its head, target,
//! and power target moved forward to them, so a node is never sent back to the backup's
//! configuration without a plan and no version is issued twice; and where it runs, so it is not
//! placed a second time. Each recovered row is recorded with an effect naming the node and the
//! signature, in the transaction that writes it.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Advance, Clock, EffectWrite as _, IntentRecovery, Opened, PlacementRecovery,
    PlacementWrite as _, RecipeSource, RecoveredFrom, RecoveryWrite as _, Sealed, Store, Tx as _,
    WorkloadWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::placement::{BoundBy, Placement};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::schedule::{Explanation, PlacementPolicy, reservation};
use korpis_core::tenancy::Name;
use korpis_core::workload::Workload;
use serde_json::{Map, Value};
use std::fmt::Write as _;

/// What one presented assignment recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Something the store lacked or held older was recorded.
    Recovered,
    /// The store held all of it already.
    Held,
    /// It could not be recovered, and an effect says why.
    Refused,
    /// A residual assignment, or one of a workload deleted before the backup: there is no
    /// declaration in it to recover.
    Skipped,
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// `e` with `more` among what it records.
fn with(mut e: Effect, more: Map<String, Value>) -> Effect {
    e.after.get_or_insert_with(Map::new).extend(more);
    e
}

/// An effect of the restore about `opened`'s workload, naming the node and the signature.
fn effect(action: &str, outcome: Outcome, opened: &Opened, sealed: &Sealed) -> Effect {
    let mut e = Effect::new(Principal::System(SystemActor::Bootstrap), action, outcome);
    e.workload = Some(opened.workload);
    e.node = Some(opened.node);
    e.after(details([
        ("node", opened.node.to_string().into()),
        ("key", sealed.key_id.clone().into()),
        ("signature", hex(&sealed.signature).into()),
        ("generation", opened.generation.into()),
        ("intent", opened.target.id.to_string().into()),
        ("intent_version", opened.target.version.into()),
        ("power_version", opened.power.power.version.into()),
    ]))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Recovers what `opened`, verified from `sealed`, says that the store lacks or holds older.
    pub(super) async fn recover(
        &self,
        opened: &Opened,
        sealed: &Sealed,
    ) -> Result<Recovery, AppError> {
        if opened.residual {
            return Ok(Recovery::Skipped);
        }
        let mut tx = self.store.begin().await?;
        let done = |action| effect(action, Outcome::Succeeded, opened, sealed);
        let mut effects = Vec::new();
        match tx.lock_workload(opened.workload).await? {
            Some(w) if w.deleted_at.is_some() => return Ok(Recovery::Skipped),
            Some(_) => {}
            None => match self.recover_workload(&mut tx, opened).await? {
                Ok(()) => effects.push(done("restore.workload.recovered")),
                Err(reason) => return self.unrecovered(tx, opened, sealed, reason).await,
            },
        }
        let from = RecoveredFrom {
            node: opened.node,
            key_id: sealed.key_id.clone(),
            signature: sealed.signature.clone(),
            generation: opened.generation,
            at: self.clock.now(),
        };
        match tx.recover_intent(&opened.target, &from).await? {
            IntentRecovery::Recorded => effects.push(done("restore.intent.recovered")),
            IntentRecovery::Held => {}
            IntentRecovery::VersionTaken => {
                drop(tx);
                let tx = self.store.begin().await?;
                return self.unrecovered(tx, opened, sealed, "version").await;
            }
        }
        let power = opened.power.power.version;
        if power > 0 && tx.recover_power(&opened.power, &from).await? {
            effects.push(done("restore.power.recovered"));
        }
        let target = (opened.target.id, opened.target.version);
        let moved =
            tx.advance_recovered(Advance { workload: opened.workload, target, power }).await?;
        if moved.head || moved.target || moved.power {
            effects.push(with(
                done("restore.targets.recovered"),
                details([
                    ("head", moved.head.into()),
                    ("target", moved.target.into()),
                    ("power_target", moved.power.into()),
                ]),
            ));
        }
        effects.extend(self.recover_where(&mut tx, opened, sealed).await?);
        if effects.is_empty() {
            return Ok(Recovery::Held);
        }
        for e in &effects {
            tx.append_effect(e).await?;
        }
        tx.commit().await?;
        Ok(Recovery::Recovered)
    }

    /// Where `opened`'s workload runs, recovered (Finding 96 of docs/23-walkthroughs.md): its
    /// placement bound to the presenting node unless it is there or a node that presented a
    /// higher lease epoch holds it, and its id slot there the one the node runs it under. Takes
    /// the placement lock, after the workload's, as every placement does.
    async fn recover_where(
        &self,
        tx: &mut S::Tx,
        opened: &Opened,
        sealed: &Sealed,
    ) -> Result<Vec<Effect>, AppError> {
        tx.lock_placement().await?;
        let mut effects = Vec::new();
        let placement = Placement {
            workload: opened.workload,
            node: opened.node,
            intent: opened.target.id,
            reserved: reservation(&opened.target.body),
            bound_at: self.clock.now(),
            bound_by: BoundBy::Recovery,
            reason: "recovered from the assignment its node presented after a restore".into(),
            explanation: Explanation {
                candidates: 1,
                filtered: Vec::new(),
                scored: Vec::new(),
                chosen: Some(opened.node),
                runner_up: None,
                policy: PlacementPolicy::default(),
            },
            authoritative: true,
        };
        let epoch = opened.lease_epoch;
        if let PlacementRecovery::Bound { replaced } =
            tx.recover_placement(&placement, epoch).await?
        {
            let replaced = replaced.map_or(Value::Null, |n| n.to_string().into());
            let more = details([("lease_epoch", epoch.into()), ("replaced", replaced)]);
            let done = effect("restore.placement.recovered", Outcome::Succeeded, opened, sealed);
            effects.push(with(done, more));
        }
        if let Some(slot) = opened.ids.slot()
            && tx.recover_id_slot(opened.node, opened.workload, slot).await?
        {
            let done = effect("restore.id_slot.recovered", Outcome::Succeeded, opened, sealed);
            effects.push(with(done, details([("slot", slot.into())])));
        }
        Ok(effects)
    }

    /// Records the workload `opened` names, which the store lacks, under its signed owner and
    /// name, or under its name followed by its identifier's first eight hex digits if a workload
    /// the store holds took the name meanwhile; why not, if neither can be.
    async fn recover_workload(
        &self,
        tx: &mut S::Tx,
        opened: &Opened,
    ) -> Result<Result<(), &'static str>, AppError> {
        let Some(project) = self.store.project(opened.project).await? else {
            return Ok(Err("project"));
        };
        if project.organization != opened.organization {
            return Ok(Err("owner"));
        }
        let Some(name) = opened.name.clone() else { return Ok(Err("name")) };
        let suffix = &opened.workload.simple().to_string()[..8];
        let fallback = Name::parse(format!("{}-{suffix}", name.as_str())).ok();
        let now = self.clock.now();
        for name in std::iter::once(name).chain(fallback) {
            let workload = Workload {
                id: opened.workload,
                project: opened.project,
                organization: opened.organization,
                name,
                labels: opened.labels.clone(),
                intent: opened.target.id,
                intent_version: 0,
                target: None,
                target_version: 0,
                power_version: 0,
                power_target_version: 0,
                version: 1,
                created: now,
                deleted_at: None,
                set: None,
                classification: Vec::new(),
                unreproducible: None,
            };
            if tx.recover_workload(&workload).await? {
                return Ok(Ok(()));
            }
        }
        Ok(Err("name"))
    }

    /// Records, in `tx`, that `opened` was not recovered, and why.
    async fn unrecovered(
        &self,
        mut tx: S::Tx,
        opened: &Opened,
        sealed: &Sealed,
        reason: &str,
    ) -> Result<Recovery, AppError> {
        let mut e = effect("restore.recovery.refused", Outcome::Failed, opened, sealed);
        e.after.get_or_insert_with(Default::default).insert("reason".into(), reason.into());
        tx.append_effect(&e).await?;
        tx.commit().await?;
        Ok(Recovery::Refused)
    }
}
