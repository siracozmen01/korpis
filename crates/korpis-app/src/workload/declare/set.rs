// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declaring through a declared set (§3.3 of docs/01-model.md, §5 and §6 of
//! docs/13-surface-cli.md). The set is found, or created in advisory mode by the first
//! declaration through it; the rule of `korpis_core::workload::set::through` decides what the
//! declaration does to the workload; and the decision is recorded: a set created, a workload
//! adopted, an out-of-band change accepted or discarded with its author named.

use super::{Base, Declared, SetDeclaration};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, EffectWrite, RecipeSource, SetWrite, Store, StoreError, Unique};
use korpis_core::audit::Outcome;
use korpis_core::tenancy::Name;
use korpis_core::workload::set::{DeclaredSet, DriftMode, SetRefusal, Through, through};
use korpis_core::workload::spec::IntentBody;
use uuid::Uuid;

/// The set a declaration goes through, and whether this declaration creates it.
#[derive(Debug, Clone)]
pub(super) struct Via {
    pub(super) set: DeclaredSet,
    pub(super) new: bool,
}

/// How a declaration goes through a set: what the set is, and what it does to the workload.
#[derive(Debug, Clone)]
pub(super) struct Passage {
    pub(super) via: Via,
    /// `None` for a workload the declaration creates.
    pub(super) through: Option<Through>,
}

fn refusal(r: &SetRefusal) -> AppError {
    let reason = match r {
        SetRefusal::OtherSet => "set.other_set",
        SetRefusal::NotAMember => "set.not_a_member",
        SetRefusal::Drifted => "set.drifted",
    };
    AppError::conflict(reason, r.to_string())
}

impl Passage {
    /// The set's identifier, which the new intent records as `managed_by`.
    pub(super) fn set_id(&self) -> Uuid {
        self.via.set.id
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The set of `name` in the workload's project, or a new advisory one.
    async fn via(&self, caller: &Caller, base: &Base, name: &Name) -> Result<Via, AppError> {
        let project = base.workload.project;
        if let Some(set) = self.store.declared_set(project, name.as_str()).await? {
            return Ok(Via { set, new: false });
        }
        let set = DeclaredSet {
            id: Uuid::now_v7(),
            project,
            name: name.clone(),
            mode: DriftMode::Advisory,
            version: 1,
            created_by: caller.principal.clone(),
            created_at: self.clock.now(),
        };
        Ok(Via { set, new: true })
    }

    /// How a declaration of `body` goes through the set `name`, or why it may not.
    pub(super) async fn passage(
        &self,
        caller: &Caller,
        base: &Base,
        through_set: &SetDeclaration,
        body: &IntentBody,
    ) -> Result<Passage, AppError> {
        let via = self.via(caller, base, &through_set.name).await?;
        let (adopt, reconcile) = (through_set.adopt, through_set.reconcile);
        let Some(head) = &base.head else {
            return Ok(Passage { via, through: None });
        };
        let same = head.body == *body;
        let t = through(&via.set, &base.workload, head, same, adopt, reconcile)
            .map_err(|r| refusal(&r))?;
        Ok(Passage { via, through: Some(t) })
    }

    /// Creates the set if the declaration is its first; before the intent that names it.
    pub(super) async fn create_set(&self, tx: &mut S::Tx, p: &Passage) -> Result<(), AppError> {
        if !p.via.new {
            return Ok(());
        }
        tx.insert_declared_set(&p.via.set).await.map_err(|e| match e {
            StoreError::Duplicate(Unique::SetName) => AppError::conflict(
                "set.created_meanwhile",
                "another declaration created the set; declare again",
            ),
            other => other.into(),
        })
    }

    /// Writes what going through the set changed, and the effects recording it.
    pub(super) async fn record_passage(
        &self,
        tx: &mut S::Tx,
        caller: &Caller,
        base: &Base,
        d: &Declared,
        p: &Passage,
    ) -> Result<(), AppError> {
        let set = &p.via.set;
        let effect = |action: &'static str, extra: Vec<(&'static str, serde_json::Value)>| {
            let mut fields =
                vec![("set", set.id.to_string().into()), ("name", set.name.as_str().into())];
            fields.extend(extra);
            let mut e = caller
                .effect(action, Outcome::Succeeded)
                .under(base.permit.grant)
                .after(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect());
            e.workload = Some(d.workload.id);
            e
        };
        if p.via.new {
            tx.append_effect(&effect("set.create", vec![("mode", set.mode.as_str().into())]))
                .await?;
        }
        let head = base.head.as_ref();
        let discarded = || {
            let h = head.map_or_else(String::new, |h| h.id.to_string());
            let by = head.map_or_else(String::new, |h| h.declared_by.key());
            vec![("intent", h.into()), ("declared_by", by.into())]
        };
        match p.through {
            Some(Through::Adopted) => {
                if !tx.join_set(d.workload.id, set.id).await? {
                    return Err(refusal(&SetRefusal::OtherSet));
                }
                tx.append_effect(&effect("set.adopt", Vec::new())).await?;
            }
            Some(Through::DriftAdopted) => {
                tx.append_effect(&effect("set.drift_adopted", discarded())).await?;
            }
            Some(Through::DriftDiscarded) => {
                tx.append_effect(&effect("set.drift_discarded", discarded())).await?;
            }
            Some(Through::Member) | None => {}
        }
        Ok(())
    }
}
