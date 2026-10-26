// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading workloads (§4.3 of docs/02-architecture.md): everything a client needs to render one
//! honestly, and the intent chain. Existence is gated by read authority (§8 of docs/10-api.md).

use super::explain::{Found, PlacementView, UnplacedView};
use super::observed::Observed;
use super::withhold::Shown;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::{Page, Paged};
use crate::ports::{Clock, RecipeSource, Store, WorkloadFilter};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::labels::LabelSelector;
use korpis_core::tenancy::Name;
use korpis_core::workload::set::drifted;
use korpis_core::workload::status::{Convergence, active_plan};
use korpis_core::workload::{Intent, Plan, PowerRecord, Workload};
use uuid::Uuid;

/// One workload as a client renders it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkloadView {
    /// The workload.
    pub workload: Workload,
    /// Its newest power declaration.
    pub power: PowerRecord,
    /// The newest declared intent.
    pub head: Intent,
    /// The approved intent, once there is one.
    pub target: Option<Intent>,
    /// The plan proposed, approved, or applying, if any.
    pub active_plan: Option<Plan>,
    /// How far it is from what was approved.
    pub convergence: Convergence,
    /// Where it is placed, as the caller may see it.
    pub placement: Option<PlacementView>,
    /// Why its target has no node, if the newest attempt found none.
    pub unplaced: Option<UnplacedView>,
    /// What its node last reported, and whether that is current.
    pub observation: Option<Observed>,
    /// Present when it belongs to a set and its head was declared outside it.
    pub drift: Option<Drift>,
    /// Its endpoints as placed, each with the addresses its node advertises, which a client
    /// reaches the ports at (§3 of docs/07-networking.md).
    pub endpoints: Vec<(korpis_core::workload::endpoint::Endpoint, Vec<std::net::IpAddr>)>,
    /// The certificate each of its ingress endpoints is served with, by endpoint name, once its
    /// names are routed (§3.2 of docs/07-networking.md).
    pub certificates: Vec<(String, korpis_core::dns::certificate::Certificate)>,
    /// Each name its ingress endpoints route, in DNS, by endpoint name (§3.5 of
    /// docs/07-networking.md).
    pub names: Vec<(String, korpis_core::dns::published::EndpointRecord)>,
    /// The configuration keys whose values were left out of its head and target, as secrets the
    /// caller may not read (§5.2 of docs/09-recipes.md).
    pub withheld: Withheld,
    /// Its newest run, for a workload that has runs (§4 of docs/05-scheduling.md).
    pub newest_run: Option<korpis_core::workload::run::Run>,
    /// Whether a suspension of its organization, or of one above it, holds it stopped whatever
    /// power it declares (§2.1 of docs/08-identity.md).
    pub held_by_suspension: bool,
    /// Its dependencies: what each reaches and how, and what each names and does not reach
    /// (§5 of docs/07-networking.md).
    pub dependencies: Vec<korpis_core::workload::dependency::DependencyStatus>,
    /// Each of its rules naming workloads: what it matched, and what it names at no address on
    /// its node, with why (§6 of docs/07-networking.md); none while it is not placed.
    pub named_rules: Vec<korpis_core::workload::peers::Resolved>,
    /// The dedicated address it holds, if any, and whether it waits on its router to be routed
    /// to its node (§3.7 of docs/07-networking.md).
    pub dedicated: Option<(korpis_core::address::Holding, bool)>,
}

/// Its endpoints as placed, each with the addresses a client reaches it at, and the dedicated
/// address it holds, with whether that waits on its router.
struct Reached {
    endpoints: Vec<(korpis_core::workload::endpoint::Endpoint, Vec<std::net::IpAddr>)>,
    dedicated: Option<(korpis_core::address::Holding, bool)>,
}

/// The keys whose values a view left out, by intent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Withheld {
    /// Of the head.
    pub head: Vec<String>,
    /// Of the target.
    pub target: Vec<String>,
}

/// A member of a set whose head was declared outside it (§6 of docs/13-surface-cli.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    /// The set.
    pub set: Uuid,
    /// The newest intent the set declared, which its files still say.
    pub set_intent: Option<Uuid>,
    /// The effect that recorded declaring the head outside the set.
    pub effect: Option<Uuid>,
}

/// Which workloads to list: in a project or under an organization, matching a selector.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListWorkloads {
    /// In this project.
    pub project: Option<Uuid>,
    /// Owned by this organization.
    pub organization: Option<Uuid>,
    /// Matching these labels.
    pub selector: LabelSelector,
    /// Members of the declared set of this name; needs `project`.
    pub set: Option<Name>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Each name the workload's ingress `endpoints` route, by endpoint name.
    async fn endpoint_names(
        &self,
        id: Uuid,
        endpoints: &[(korpis_core::workload::endpoint::Endpoint, Vec<std::net::IpAddr>)],
    ) -> Result<Vec<(String, korpis_core::dns::published::EndpointRecord)>, AppError> {
        let mut names = Vec::new();
        for r in self.store.endpoint_records(id).await? {
            if let Some((e, _)) = endpoints.iter().find(|(e, _)| e.id == r.endpoint) {
                names.push((e.name.clone(), r));
            }
        }
        Ok(names)
    }

    /// `id`'s endpoints as placed, each with the addresses its node advertises, and the
    /// dedicated address it holds, placed on `node`.
    async fn endpoints_advertised(
        &self,
        id: Uuid,
        node: Option<Uuid>,
    ) -> Result<Reached, AppError> {
        use korpis_core::workload::spec::endpoints::Exposure;
        let mut endpoints = Vec::new();
        let held = self.store.dedicated_of(id).await?;
        let dedicated = held.as_ref().map(|h| h.address);
        let stable = self.store.stable_of(id).await?;
        for e in self.store.endpoints(id).await? {
            // A `dedicated_ip` endpoint is reached at its workload's own address alone.
            if e.exposure == Exposure::DedicatedIp {
                endpoints.push((e, dedicated.into_iter().collect()));
                continue;
            }
            // A `stable` endpoint an edge serves, at its group's address and port (§3.1 of
            // docs/07-networking.md), which no port of its node is.
            if e.exposure == Exposure::Stable {
                let at: Vec<_> = stable.iter().filter(|p| p.endpoint == e.name).collect();
                let port = at.first().map(|p| p.port);
                let addresses = at.iter().map(|p| p.address).collect();
                endpoints
                    .push((korpis_core::workload::endpoint::Endpoint { port, ..e }, addresses));
                continue;
            }
            let node = self.store.node(e.node).await?;
            let addresses = node.and_then(|n| n.advertised).map(|a| a.addresses);
            endpoints.push((e, addresses.unwrap_or_default()));
        }
        let dedicated = held.map(|h| {
            let waits = h.waits_on_router(node);
            (h, waits)
        });
        Ok(Reached { endpoints, dedicated })
    }

    async fn unrendered(&self, workload: Workload) -> Result<(WorkloadView, Found), AppError> {
        let id = workload.id;
        let missing = |what: &str| AppError::Internal(format!("workload {id} has no {what}"));
        let power = self.store.power(id).await?.ok_or_else(|| missing("power"))?;
        let head = self.store.intent(workload.intent).await?.ok_or_else(|| missing("head"))?;
        let target = match workload.target {
            Some(t) => Some(self.store.intent(t).await?.ok_or_else(|| missing("target"))?),
            None => None,
        };
        let open = self.store.open_plans(id).await?;
        let active = active_plan(&open).cloned();
        let found = self.found(&workload, target.as_ref()).await?;
        let placed = found.placed_for(workload.target);
        let (convergence, observation) =
            self.measure(&workload, &open, found.node(), placed).await?;
        let drift = match workload.set.filter(|_| drifted(&workload, &head)) {
            Some(set) => Some(Drift {
                set,
                set_intent: self.store.set_intent(id, set).await?,
                effect: self.store.declaring_effect(id, head.id).await?,
            }),
            None => None,
        };
        let Reached { endpoints, dedicated } = self.endpoints_advertised(id, found.node()).await?;
        let mut certificates = Vec::new();
        for (endpoint, c) in self.store.routed_certificates(id).await? {
            if let Some(found) = self.store.certificate(c).await? {
                certificates.push((endpoint, found));
            }
        }
        let names = self.endpoint_names(id, &endpoints).await?;
        let newest = crate::page::Page { after: None, size: 1 };
        let newest_run = self.store.runs(id, newest).await?.into_iter().next();
        let dependencies = self.dependency_status(&workload, target.as_ref()).await?;
        let named_rules = self.named_rule_status(&workload, target.as_ref()).await?;
        let view = WorkloadView {
            dedicated,
            dependencies,
            named_rules,
            newest_run,
            names,
            certificates,
            endpoints,
            drift,
            held_by_suspension: self.held_by(workload.organization).await?.is_some(),
            workload,
            power,
            head,
            target,
            active_plan: active,
            convergence,
            placement: None,
            unplaced: None,
            observation,
            withheld: Withheld::default(),
        };
        Ok((view, found))
    }

    /// Views of `workloads`, their placements filtered for the caller.
    async fn views(
        &self,
        caller: &Caller,
        workloads: Vec<Workload>,
    ) -> Result<Vec<WorkloadView>, AppError> {
        let (mut views, mut found) = (Vec::new(), Vec::new());
        for w in workloads {
            let (v, f) = self.unrendered(w).await?;
            views.push(v);
            found.push(f);
        }
        self.render_placements(caller, &mut views, found).await?;
        self.withhold_views(caller, &mut views).await?;
        Ok(views)
    }

    /// One workload the caller may read, deleted or not.
    pub async fn get_workload(&self, caller: &Caller, id: Uuid) -> Result<WorkloadView, AppError> {
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, id)).await?;
        self.workload_view(caller, id).await
    }

    /// The view of a workload whose read has been authorized.
    pub(crate) async fn workload_view(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<WorkloadView, AppError> {
        let w = self.store.workload(id).await?.ok_or(AppError::NotFound { kind: "workload" })?;
        let mut views = self.views(caller, vec![w]).await?;
        views.pop().ok_or_else(|| AppError::Internal(format!("workload {id} rendered nothing")))
    }

    /// The workloads of a project, or under an organization, that the caller may read. A page
    /// may hold fewer than its size when the selector skips some; its cursor still continues
    /// after the last workload read, so no workload is skipped.
    pub async fn list_workloads(
        &self,
        caller: &Caller,
        q: &ListWorkloads,
        page: Page,
    ) -> Result<Paged<WorkloadView>, AppError> {
        let within = match (q.project, q.organization) {
            (Some(p), _) => ObjectRef::new(Kind::Project, p),
            (None, Some(o)) => ObjectRef::new(Kind::Organization, o),
            (None, None) => {
                return Err(AppError::invalid("project", "a project or an organization"));
            }
        };
        self.authorize(caller, "workload.read", within).await?;
        let set = match (&q.set, q.project) {
            (None, _) => None,
            (Some(_), None) => return Err(AppError::invalid("set", "a set is named in a project")),
            (Some(n), Some(p)) => match self.store.declared_set(p, n.as_str()).await? {
                Some(s) => Some(s.id),
                None => return Ok(Paged { items: Vec::new(), next: None }),
            },
        };
        let filter = WorkloadFilter { project: q.project, organization: q.organization, set };
        let read = self.store.workloads(&filter, page).await?;
        let next = page.next(&read, |w| w.id);
        let matching = read.into_iter().filter(|w| q.selector.matches(w.labels.as_map())).collect();
        Ok(Paged { items: self.views(caller, matching).await?, next })
    }

    /// A workload's intents, oldest first, as the caller is shown them.
    pub async fn list_intents(
        &self,
        caller: &Caller,
        workload: Uuid,
        page: Page,
    ) -> Result<Paged<Shown>, AppError> {
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, workload)).await?;
        let items = self.store.intents(workload, page).await?;
        let next = page.next(&items, |i| i.id);
        Ok(Paged { items: self.shown(caller, workload, items).await?, next })
    }
}
