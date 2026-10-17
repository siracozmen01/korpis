// SPDX-License-Identifier: AGPL-3.0-or-later

//! A secret field's value, read back only with `workload.secret.read` (§5.2 of
//! docs/09-recipes.md). Every intent a read returns has the values of its recipe's secret fields
//! withheld from a caller who may not read them, and names the keys it withheld, so a client
//! knows a value is set without learning it. What is secret is the recipe's to say, and the
//! recipe is the one the intent pins by digest, so the answer never changes for an intent; a
//! recipe that cannot be read withholds every value, since what it marks secret is then unknown.

use super::read::WorkloadView;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::Intent;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// An intent as a caller is shown it.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    /// The intent, with every value the caller may not read left out of its configuration.
    pub intent: Intent,
    /// The keys whose values were left out.
    pub withheld: Vec<String>,
}

/// What is secret in each recipe met while answering one read, by digest; `None` where the
/// recipe could not be read.
type Secrets = BTreeMap<String, Option<BTreeSet<String>>>;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Whether `caller` may read secret values on each workload of `ids`.
    async fn reads_secrets(&self, caller: &Caller, ids: &[Uuid]) -> Result<Vec<bool>, AppError> {
        let objects: Vec<ObjectRef> =
            ids.iter().map(|id| ObjectRef::new(Kind::Workload, *id)).collect();
        self.permitted_each(caller, "workload.secret.read", &objects).await
    }

    /// Leaves out of `intent` the values of the fields its recipe marks secret; the keys.
    async fn withhold_one(&self, intent: &mut Intent, secrets: &mut Secrets) -> Vec<String> {
        let r = &intent.body.recipe;
        if !secrets.contains_key(&r.digest) {
            let found = self.described_recipe(&r.name, &r.digest).await.ok().map(|(_, d)| {
                d.recipe
                    .config
                    .fields()
                    .iter()
                    .filter(|f| f.secret)
                    .map(|f| f.key.clone())
                    .collect()
            });
            secrets.insert(r.digest.clone(), found);
        }
        let secret = secrets.get(&r.digest).and_then(Option::as_ref);
        let withheld: Vec<String> = intent
            .body
            .config
            .keys()
            .filter(|k| secret.is_none_or(|s| s.contains(*k)))
            .cloned()
            .collect();
        for k in &withheld {
            intent.body.config.remove(k);
        }
        withheld
    }

    /// `intents` of workload `workload` as `caller` is shown them.
    pub(crate) async fn shown(
        &self,
        caller: &Caller,
        workload: Uuid,
        intents: Vec<Intent>,
    ) -> Result<Vec<Shown>, AppError> {
        let reads = self.reads_secrets(caller, &[workload]).await?.first().copied() == Some(true);
        let mut secrets = Secrets::new();
        let mut out = Vec::with_capacity(intents.len());
        for mut intent in intents {
            let withheld =
                if reads { Vec::new() } else { self.withhold_one(&mut intent, &mut secrets).await };
            out.push(Shown { intent, withheld });
        }
        Ok(out)
    }

    /// Withholds from each of `views` what its caller may not read of its head and target.
    pub(crate) async fn withhold_views(
        &self,
        caller: &Caller,
        views: &mut [WorkloadView],
    ) -> Result<(), AppError> {
        let ids: Vec<Uuid> = views.iter().map(|v| v.workload.id).collect();
        let reads = self.reads_secrets(caller, &ids).await?;
        let mut secrets = Secrets::new();
        for (v, reads) in views.iter_mut().zip(reads) {
            if reads {
                continue;
            }
            v.withheld.head = self.withhold_one(&mut v.head, &mut secrets).await;
            if let Some(t) = v.target.as_mut() {
                v.withheld.target = self.withhold_one(t, &mut secrets).await;
            }
        }
        Ok(())
    }
}
