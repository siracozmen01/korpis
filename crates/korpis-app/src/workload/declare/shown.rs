// SPDX-License-Identifier: AGPL-3.0-or-later

//! A declaration's secrets (§5.2 of docs/09-recipes.md): kept from the head for a caller that may
//! not read them, and withheld from what a declaration answers.

use super::{Base, Declared};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::declare::Declaration;
use korpis_core::workload::spec::IntentBody;

/// `declaration` with each of `keep` carried from `base`'s head as it is. A kept key is one the
/// caller leaves out because it may not read it, so naming it in the declaration too is a
/// mistake, and keeping needs a head to keep from.
pub(super) fn kept(
    base: &Base,
    mut declaration: Declaration,
    keep: &[String],
) -> Result<Declaration, AppError> {
    if keep.is_empty() {
        return Ok(declaration);
    }
    let head = base
        .head
        .as_ref()
        .ok_or_else(|| AppError::invalid("keep", "only a change to an existing workload keeps"))?;
    for key in keep {
        if declaration.config.contains_key(key) {
            return Err(AppError::invalid("keep", format!("{key} is both kept and declared")));
        }
        if let Some(v) = head.body.config.get(key) {
            declaration.config.insert(key.clone(), v.clone());
        }
    }
    Ok(declaration)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Checks that keeping `keep` reveals nothing to `caller`. A kept value goes where the recipe
    /// it is declared with sends it, and only the recipe the value was set under is known to keep
    /// it out of the caller's sight: another, even one that marks the field secret, could deliver
    /// it to a command that prints it to a console the caller reads, encoded past redaction. So
    /// keeping a value into any recipe but the head's, by digest, needs `workload.secret.read` on
    /// the workload, as reading it would.
    pub(super) async fn check_kept(
        &self,
        caller: &Caller,
        base: &Base,
        keep: &[String],
        declared: &IntentBody,
    ) -> Result<(), AppError> {
        let same =
            base.head.as_ref().is_some_and(|h| h.body.recipe.digest == declared.recipe.digest);
        if keep.is_empty() || same {
            return Ok(());
        }
        let workload = ObjectRef::new(Kind::Workload, base.workload.id);
        self.authorize(caller, "workload.secret.read", workload).await.map(|_| ())
    }

    /// `declared` as `caller` is shown it.
    pub(super) async fn declared_shown(
        &self,
        caller: &Caller,
        mut declared: Declared,
    ) -> Result<Declared, AppError> {
        let id = declared.workload.id;
        let shown = self.shown(caller, id, vec![declared.intent]).await?.pop();
        let shown = shown.ok_or_else(|| AppError::Internal(format!("workload {id} shown none")))?;
        declared.intent = shown.intent;
        declared.withheld = shown.withheld;
        Ok(declared)
    }
}
