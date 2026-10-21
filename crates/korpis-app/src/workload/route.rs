// SPDX-License-Identifier: AGPL-3.0-or-later

//! The names a workload's `ingress` endpoints route (§3.2 of docs/07-networking.md). Each must be
//! held by the workload's organization, which is the holder of the most specific verified domain
//! covering it, and routed by no other workload in the installation. Both are checked at
//! declaration, from the records as they are, and again when a plan moves the target, under the
//! domains lock, where the names are claimed: the declaration's check answers early, and the
//! claim is what holds. A route records the certificate its endpoint serves with, which must
//! serve every name the endpoint routes, checked the same two times.

use crate::App;
use crate::certificate::{named, servable};
use crate::error::AppError;
use crate::ports::{CertificateWrite as _, Clock, DomainWrite as _, PortalWrite as _};
use crate::ports::{RecipeSource, Store};
use korpis_core::dns::certificate::Owner;
use korpis_core::dns::domain::{DomainName, Route, holder_of};
use korpis_core::workload::Workload;
use korpis_core::workload::spec::IntentBody;
use korpis_core::workload::spec::endpoints::{Exposure, TlsSpec};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The routes `body` declares for `workload` of `organization`.
fn routes_in(
    (organization, workload): (Uuid, Uuid),
    body: &IntentBody,
) -> Result<Vec<Route>, AppError> {
    let mut routes = Vec::new();
    for (i, e) in body.endpoints.iter().enumerate().filter(|(_, e)| e.exposure == Exposure::Ingress)
    {
        let certificate = named(&format!("endpoints[{i}]"), e)?;
        routes.extend(e.domains.iter().filter_map(|d| DomainName::parse(d)).map(|name| Route {
            name,
            organization,
            workload,
            endpoint: e.name.clone(),
            certificate,
        }));
    }
    Ok(routes)
}

/// The names each endpoint of `body` routes whose certificate the control plane orders, sorted.
fn acme_names(body: &IntentBody) -> Vec<(String, Vec<String>)> {
    body.endpoints
        .iter()
        .filter(|e| e.exposure == Exposure::Ingress && matches!(e.tls, None | Some(TlsSpec::Acme)))
        .map(|e| {
            let mut names = e.domains.clone();
            names.sort();
            (e.name.clone(), names)
        })
        .collect()
}

/// The certificates `routes` name, each with the names served with it.
fn served(routes: &[Route]) -> BTreeMap<Uuid, Vec<DomainName>> {
    let mut by = BTreeMap::<Uuid, Vec<DomainName>>::new();
    for r in routes {
        if let Some(c) = r.certificate {
            by.entry(c).or_default().push(r.name.clone());
        }
    }
    by
}

fn not_held(name: &DomainName) -> AppError {
    AppError::conflict(
        "endpoint.name_not_held",
        format!(
            "{} is not the organization's to route: no verified domain of it is the most \
             specific covering the name",
            name.as_str()
        ),
    )
}

fn portal(name: &DomainName) -> AppError {
    AppError::conflict(
        "endpoint.name_portal",
        format!(
            "{} is a portal, which the control plane answers under itself (§3.6 of \
             docs/07-networking.md)",
            name.as_str()
        ),
    )
}

fn routed(name: &DomainName) -> AppError {
    AppError::conflict(
        "endpoint.name_routed",
        format!("{} is routed by another workload's endpoint", name.as_str()),
    )
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Checks at declaration that the organization holds every name `body` routes and that no
    /// other workload routes one.
    pub(super) async fn check_routes(
        &self,
        owner: (Uuid, Uuid),
        body: &IntentBody,
    ) -> Result<(), AppError> {
        let routes = routes_in(owner, body)?;
        for r in &routes {
            let covering = self.store.verified_covering(&r.name).await?;
            if holder_of(&r.name, &covering) != Some(owner.0) {
                return Err(not_held(&r.name));
            }
        }
        let now = self.clock.now();
        for (id, names) in served(&routes) {
            let found = self.store.certificate(id).await?;
            servable((id, found.as_ref()), owner.0, &names, now)?;
        }
        let names: Vec<DomainName> = routes.into_iter().map(|r| r.name).collect();
        if let Some(p) = self.store.portals_named(&names).await?.first() {
            return Err(portal(&p.name));
        }
        let taken = self.store.routes_of(&names).await?;
        match taken.iter().find(|r| r.workload != owner.1) {
            Some(r) => Err(routed(&r.name)),
            None => Ok(()),
        }
    }

    /// Claims what `body`, the workload's new target, holds: the existing volumes it attaches
    /// and the names it routes.
    pub(crate) async fn claim_target(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
        body: &IntentBody,
    ) -> Result<(), AppError> {
        self.claim_existing(tx, w, body).await?;
        self.claim_routes(tx, w, body).await
    }

    /// Claims the names `body`, the workload's new target, routes, in `tx`: under the domains
    /// lock, so that no domain is deleted or delegated away beneath a claim being made.
    async fn claim_routes(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
        body: &IntentBody,
    ) -> Result<(), AppError> {
        let routes = routes_in((w.organization, w.id), body)?;
        tx.lock_domains().await?;
        for r in &routes {
            let covering = tx.verified_overlapping(&r.name).await?;
            if holder_of(&r.name, &covering) != Some(w.organization) {
                return Err(not_held(&r.name));
            }
        }
        let names: Vec<DomainName> = routes.iter().map(|r| r.name.clone()).collect();
        if let Some(p) = tx.portals_named_now(&names).await?.first() {
            return Err(portal(&p.name));
        }
        let now = self.clock.now();
        for (id, names) in served(&routes) {
            let found = tx.lock_certificate(id, false).await?;
            servable((id, found.as_ref()), w.organization, &names, now)?;
        }
        let mut routes = routes;
        for (endpoint, names) in acme_names(body) {
            let owner = Owner { workload: w.id, endpoint };
            let id = tx.acme_certificate(w.organization, &owner, &names, now).await?;
            for r in routes.iter_mut().filter(|r| r.endpoint == owner.endpoint) {
                r.certificate = Some(id);
            }
        }
        match tx.claim_routes(w.id, &routes, self.clock.now()).await? {
            Some(taken) => Err(routed(&taken)),
            None => Ok(()),
        }
    }
}
