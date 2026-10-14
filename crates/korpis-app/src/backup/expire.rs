// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deleting offloaded stream segments past their retention (§4 of docs/14-streams.md): how long
//! the workload's stream declares, from when the control plane heard each was offloaded, or the
//! operator's disclosed floor where that is longer (§8 of docs/17-security.md). Each segment's
//! object is deleted with the target's own credential, beneath the workload's stream prefix and
//! nowhere else, and only then forgotten by the index, so a failure leaves it to the next pass
//! rather than leaving an object nothing names. A segment its node still holds stays readable
//! there; one it does not is then held nowhere, which is what its retention asked for.

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{
    Clock, EffectWrite as _, Expired, RecipeSource, Store, StreamWrite as _, Tx as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::extension::{CallOutcome, MAX_KEYS, ProviderType};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::stream::segment_object;
use std::collections::BTreeMap;
use uuid::Uuid;

/// The most segments one pass deletes.
const PASS: u32 = 1000;

/// What one pass did.
#[derive(Debug, Default)]
pub struct Expiry {
    /// Segments deleted and forgotten.
    pub deleted: u64,
    /// Why the segments of a repository and a workload were not, left to the next pass.
    pub failed: Vec<(Uuid, Uuid, AppError)>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Deletes up to a pass of offloaded segments past their retention. A repository that fails
    /// is left to the next pass, and the others go on.
    pub async fn expire_offloaded(&self) -> Result<Expiry, AppError> {
        let now = self.clock.now();
        let expired = self.store.expired_offloaded(now, self.stream_floor, PASS).await?;
        let mut by_place: BTreeMap<(Uuid, Uuid), Vec<Expired>> = BTreeMap::new();
        for e in expired {
            by_place.entry((e.repository, e.stream.0)).or_default().push(e);
        }
        let mut pass = Expiry::default();
        for ((repository, workload), segments) in by_place {
            match self.expire_in(repository, workload, &segments).await {
                Ok(n) => pass.deleted += n,
                Err(e) => pass.failed.push((repository, workload, e)),
            }
        }
        Ok(pass)
    }

    /// Deletes `segments` of `workload` from `repository`, then forgets them; how many.
    async fn expire_in(
        &self,
        repository: Uuid,
        workload: Uuid,
        segments: &[Expired],
    ) -> Result<u64, AppError> {
        let repo = self.store.repository(repository).await?;
        let repo = repo.ok_or(AppError::NotFound { kind: "repository" })?;
        let place = repo.stream_prefix(workload);
        let names: Vec<String> = segments
            .iter()
            .filter_map(|e| segment_object(e.first_offset, &e.digest))
            .map(|name| format!("{place}{name}"))
            .collect();
        let target = CallTarget {
            organization: repo.organization,
            kind: ProviderType::BackupTarget,
            name: repo.provider.clone(),
        };
        for page in names.chunks(MAX_KEYS) {
            let called = self.delete_backup_objects(&target, &place, page.to_vec()).await?;
            match called.outcome {
                CallOutcome::Deleted { .. } => {}
                CallOutcome::Failed(f) => {
                    return Err(AppError::Unavailable {
                        what: "the backup target",
                        detail: f.reason,
                    });
                }
                _ => {
                    let detail = "malformed".into();
                    return Err(AppError::Unavailable { what: "the backup target", detail });
                }
            }
        }
        let mut tx = self.store.begin().await?;
        let mut forgotten = 0u64;
        for e in segments {
            forgotten += u64::from(tx.forget_offloaded(e).await?);
        }
        let actor = Principal::System(SystemActor::Reconciler);
        let mut effect =
            Effect::new(actor, "stream.offload_expired", Outcome::Succeeded).after(details([
                ("repository", repository.to_string().into()),
                ("segments", forgotten.into()),
            ]));
        effect.workload = Some(workload);
        tx.append_effect(&effect).await?;
        tx.commit().await?;
        Ok(forgotten)
    }
}
