// SPDX-License-Identifier: AGPL-3.0-or-later

//! An installation's grants (§4 of docs/16-extensions.md): one per approved need, held by
//! `extension:<id>`, each attenuated from a grant the installer holds that covers it. The check is
//! the one every issued grant passes (§3.2 of docs/08-identity.md), so an extension can never
//! hold what its installer could not have given.

use crate::App;
use crate::authz::MAX_LABEL;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Delegation, Grant, GrantRecord, Kind, Node, ObjectRef, Resource, Scope};
use korpis_core::extension::{Need, NeedScope};
use korpis_core::principal::Principal;
use korpis_core::workload::Workload;
use uuid::Uuid;

/// Where an installation is made: the organization, the project when it is one, and the
/// workload it will run as.
pub(super) struct Place<'a> {
    pub organization: Uuid,
    pub project: Option<Uuid>,
    pub workload: &'a Workload,
}

impl Place<'_> {
    /// The object a need over `scope` is granted on.
    fn object(&self, scope: NeedScope) -> ObjectRef {
        match (scope, self.project) {
            (NeedScope::Target, Some(p)) => ObjectRef::new(Kind::Project, p),
            (NeedScope::Target | NeedScope::Organization, _) => {
                ObjectRef::new(Kind::Organization, self.organization)
            }
            (NeedScope::OwnWorkload, _) => ObjectRef::new(Kind::Workload, self.workload.id),
        }
    }
}

/// A grant to issue, and the installer's grant it is attenuated from.
pub(super) struct NeedGrant {
    pub record: GrantRecord,
    pub parent: Grant,
}

/// The label a need's grant carries: what it is for, cut to the length a label may have.
fn label(extension: &str, reason: &str) -> String {
    format!("extension {extension}: {reason}").chars().take(MAX_LABEL).collect()
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// One grant per need, each with the installer's grant that covers it; a need none covers
    /// is refused by its position in the list.
    pub(super) async fn need_grants(
        &self,
        caller: &Caller,
        (extension, name): (Uuid, &str),
        needs: &[Need],
        place: &Place<'_>,
    ) -> Result<Vec<NeedGrant>, AppError> {
        let workload = self.planned(place.workload).await?;
        let held = self.store.chains_held_by(&caller.subject.key).await?;
        let mut leaves: Vec<Uuid> =
            held.iter().filter_map(|c| c.first().map(|r| r.grant.id)).collect();
        leaves.sort_unstable();
        let holder = Principal::Extension(extension);
        let now = self.clock.now();
        let mut out = Vec::with_capacity(needs.len());
        for (i, need) in needs.iter().enumerate() {
            let mut grant = Grant {
                id: Uuid::now_v7(),
                subject: holder.subject(),
                actions: need.actions.iter().cloned().collect(),
                scope: Scope::Object(place.object(need.scope)),
                conditions: korpis_core::authz::Conditions::default(),
                parent: None,
                delegation: Delegation::default(),
                link_form: false,
                uses: 0,
                revoked: false,
                suspended: false,
                irreversible_override: false,
                root: false,
            };
            let parent =
                self.covering(caller, &mut grant, &leaves, &workload).await?.ok_or_else(|| {
                    let why = "no grant you hold can be attenuated to cover it";
                    AppError::invalid(format!("approved_needs[{i}]"), why)
                })?;
            let record = GrantRecord {
                grant,
                holder: holder.clone(),
                issued_by: caller.principal.clone(),
                issued_at: now,
                revoked_at: None,
                suspended_at: None,
                label: label(name, &need.reason),
            };
            out.push(NeedGrant { record, parent });
        }
        Ok(out)
    }

    /// The first of `leaves`, in identifier order, that `grant` is a valid attenuation of.
    async fn covering(
        &self,
        caller: &Caller,
        grant: &mut Grant,
        leaves: &[Uuid],
        workload: &Resource,
    ) -> Result<Option<Grant>, AppError> {
        for leaf in leaves {
            grant.parent = Some(*leaf);
            match self.check_issue_assuming(caller, grant, Some(workload)).await {
                Ok(parent) => return Ok(Some(parent)),
                Err(AppError::Invalid { .. } | AppError::NotFound { .. }) => {}
                Err(other) => return Err(other),
            }
        }
        grant.parent = None;
        Ok(None)
    }

    /// The ancestry the workload will have once its transaction commits.
    async fn planned(&self, workload: &Workload) -> Result<Resource, AppError> {
        let project = self.store.ancestry(ObjectRef::new(Kind::Project, workload.project)).await?;
        let node = Node {
            object: ObjectRef::new(Kind::Workload, workload.id),
            labels: workload.labels.as_map().clone(),
        };
        Ok(Resource { chain: std::iter::once(node).chain(project).collect() })
    }
}
