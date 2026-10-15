// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's dependencies, the control plane's half (§5 of docs/07-networking.md): checked
//! when declared, and resolved to the endpoints they reach whenever the workload is placed or its
//! assignment is computed. Where each endpoint's workload is, its node says.

use crate::App;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, RecipeSource, Store, WorkloadFilter};
use korpis_core::schedule::Needed;
use korpis_core::workload::dependency::{
    NotATarget, ProjectWorkload, ResolvedDependency, reachable, resolve,
};
use korpis_core::workload::spec::IntentBody;
use korpis_core::workload::spec::dependencies::DependencyTarget;
use korpis_core::workload::{Intent, Workload};
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The workloads of `project` other than `except`, each with the intent it is to run.
    async fn project_workloads(
        &self,
        project: Uuid,
        except: Uuid,
    ) -> Result<Vec<(Workload, Option<Intent>)>, AppError> {
        let filter = WorkloadFilter { project: Some(project), ..WorkloadFilter::default() };
        let mut out = Vec::new();
        let mut after = None;
        loop {
            let page = self.store.workloads(&filter, Page { after, size: Page::MAX }).await?;
            let Some(last) = page.last().map(|w| w.id) else { break };
            for w in page.into_iter().filter(|w| w.id != except) {
                let target = match w.target {
                    Some(t) => self.store.intent(t).await?,
                    None => None,
                };
                out.push((w, target));
            }
            after = Some(last);
        }
        Ok(out)
    }

    /// `body`'s dependencies, for `workload` in `project`, resolved to what they reach now.
    pub(crate) async fn resolve_dependencies(
        &self,
        (project, workload): (Uuid, Uuid),
        body: &IntentBody,
    ) -> Result<Vec<ResolvedDependency>, AppError> {
        if body.dependencies.is_empty() {
            return Ok(Vec::new());
        }
        let held = self.project_workloads(project, workload).await?;
        let read: Vec<ProjectWorkload<'_>> = held
            .iter()
            .map(|(w, i)| ProjectWorkload {
                id: w.id,
                name: w.name.as_str(),
                labels: w.labels.as_map(),
                body: i.as_ref().map(|i| &i.body),
            })
            .collect();
        let mut out = Vec::with_capacity(body.dependencies.len());
        for d in &body.dependencies {
            let endpoint = match d.target {
                DependencyTarget::Endpoint(id) => self.store.endpoint(id).await?,
                _ => None,
            };
            out.push(resolve(d, &read, endpoint.as_ref()));
        }
        Ok(out)
    }

    /// `body`'s dependencies as placement reads them.
    pub(crate) async fn needed(
        &self,
        w: &Workload,
        body: &IntentBody,
    ) -> Result<Vec<Needed>, AppError> {
        let resolved = self.resolve_dependencies((w.project, w.id), body).await?;
        Ok(resolved
            .into_iter()
            .map(|r| Needed {
                name: r.name,
                required: r.required,
                targets: r.targets.into_iter().map(|t| t.workload).collect(),
            })
            .collect())
    }

    /// Refuses a dependency of `workload` in `project` whose target is not one: a workload or
    /// an endpoint outside the project, the workload itself, or a workload without exactly one
    /// endpoint anything can reach (§5 of docs/07-networking.md).
    pub(crate) async fn check_dependencies(
        &self,
        (project, workload): (Uuid, Uuid),
        body: &IntentBody,
    ) -> Result<(), AppError> {
        for (i, d) in body.dependencies.iter().enumerate() {
            if let Some(why) = self.not_a_target((project, workload), &d.target).await? {
                return Err(AppError::invalid(format!("body.dependencies[{i}].target"), why));
            }
        }
        Ok(())
    }

    /// Why `target` is not a target for `workload` in `project`, if it is not.
    async fn not_a_target(
        &self,
        (project, workload): (Uuid, Uuid),
        target: &DependencyTarget,
    ) -> Result<Option<&'static str>, AppError> {
        const OUTSIDE: &str = "not a workload of this project, which a dependency's target is \
                               until question 9 of §11 of docs/07-networking.md is answered";
        let ours = |w: Option<Workload>| {
            w.filter(|w| w.deleted_at.is_none() && w.project == project && w.id != workload)
        };
        Ok(match target {
            DependencyTarget::Workload(id) if *id == workload => {
                Some("a workload does not depend on itself")
            }
            DependencyTarget::Workload(id) => match ours(self.store.workload(*id).await?) {
                None => Some(OUTSIDE),
                Some(w) => match w.target {
                    Some(t) => self
                        .store
                        .intent(t)
                        .await?
                        .and_then(|i| reachable(&i.body).err())
                        .map(NotATarget::as_str),
                    None => None,
                },
            },
            DependencyTarget::Endpoint(id) => match self.store.endpoint(*id).await? {
                None => Some(OUTSIDE),
                Some(e) => {
                    ours(self.store.workload(e.workload).await?).is_none().then_some(OUTSIDE)
                }
            },
            DependencyTarget::Selector(_) => None,
        })
    }
}
