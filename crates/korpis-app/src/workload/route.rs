// SPDX-License-Identifier: AGPL-3.0-or-later

//! The names a workload's `ingress` endpoints route (§3.2 of docs/07-networking.md). Each must be
//! held by the workload's organization, which is the holder of the most specific verified domain
//! covering it, and routed by no other workload in the installation. Both are checked at
//! declaration, from the records as they are, and again when a plan moves the target, under the
//! domains lock, where the names are claimed: the declaration's check answers early, and the
//! claim is what holds.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, DomainWrite as _, RecipeSource, Store};
use korpis_core::dns::domain::{DomainName, Route, holder_of};
use korpis_core::workload::Workload;
use korpis_core::workload::spec::IntentBody;
use korpis_core::workload::spec::endpoints::Exposure;
use uuid::Uuid;

/// The routes `body` declares for `workload` of `organization`.
fn routes_in((organization, workload): (Uuid, Uuid), body: &IntentBody) -> Vec<Route> {
    body.endpoints
        .iter()
        .filter(|e| e.exposure == Exposure::Ingress)
        .flat_map(|e| {
            e.domains.iter().filter_map(|d| DomainName::parse(d)).map(|name| Route {
                name,
                organization,
                workload,
                endpoint: e.name.clone(),
            })
        })
        .collect()
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
        let routes = routes_in(owner, body);
        for r in &routes {
            let covering = self.store.verified_covering(&r.name).await?;
            if holder_of(&r.name, &covering) != Some(owner.0) {
                return Err(not_held(&r.name));
            }
        }
        let names: Vec<DomainName> = routes.into_iter().map(|r| r.name).collect();
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
        let routes = routes_in((w.organization, w.id), body);
        tx.lock_domains().await?;
        for r in &routes {
            let covering = tx.verified_overlapping(&r.name).await?;
            if holder_of(&r.name, &covering) != Some(w.organization) {
                return Err(not_held(&r.name));
            }
        }
        match tx.claim_routes(w.id, &routes, self.clock.now()).await? {
            Some(taken) => Err(routed(&taken)),
            None => Ok(()),
        }
    }
}
