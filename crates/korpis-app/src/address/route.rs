// SPDX-License-Identifier: AGPL-3.0-or-later

//! Asking a pool's router to route its addresses (§3.7 and §3.1 of docs/07-networking.md). Each
//! address due is taken under a hold, so control planes share the work; its router, the
//! `address_router` provider of the pool's name that the pool's organization sees, is asked to
//! route it to the node that holds it, at that node's advertised addresses, or nowhere; and the
//! node is recorded only on the router's answer. For a cluster pool's address, the node its
//! workload is placed on, that answer is what puts the address in the node's assignment; for a
//! routed edge pool's, the member its lease was last issued to, which holds it under the lease
//! already, the answer is what the group's status shows. A failure is kept with its reason and
//! asked again on a backoff.

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{AddressRoute as _, Clock, RecipeSource, Store, Tx as _};
use korpis_core::address::{DueRoute, Route, route_backoff};
use korpis_core::extension::{CallOutcome, CallRequest, ProviderType};
use std::time::Duration;

/// How long a control plane holds the address it took.
const HOLD: Duration = Duration::from_secs(5 * 60);
/// How long a router is given to answer.
const CALL_WITHIN: Duration = Duration::from_secs(60);

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Asks the router of the next address due, if one is; whether one was.
    pub async fn route_next_address(&self) -> Result<bool, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let due = tx.take_due_route(now, now + HOLD).await?;
        tx.commit().await?;
        let Some(due) = due else { return Ok(false) };
        let failed = match self.ask_router(&due).await {
            Ok(None) => None,
            Ok(Some(why)) => Some(why),
            Err(e) => Some(e.to_string()),
        };
        let mut tx = self.store.begin().await?;
        match failed {
            None => tx.route_answered((due.of, due.address), due.wanted).await?,
            Some(why) => {
                let attempts = due.attempts.saturating_add(1);
                let again = self.clock.now() + route_backoff(attempts);
                tx.route_failed((due.of, due.address), (&why, attempts), again).await?;
            }
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Asks `due`'s router to route it; why not, where it did not.
    async fn ask_router(&self, due: &DueRoute) -> Result<Option<String>, AppError> {
        let next_hops = match due.wanted {
            Some(node) => self.store.node(node).await?.and_then(|n| n.advertised),
            None => None,
        };
        let next_hops = next_hops.map(|a| a.addresses).unwrap_or_default();
        if due.wanted.is_some() && next_hops.is_empty() {
            return Ok(Some("its node advertises no address to route it through".into()));
        }
        let route = Route { address: due.address, node: due.wanted, next_hops };
        let target = CallTarget {
            organization: due.organization,
            kind: ProviderType::AddressRouter,
            name: due.router.clone(),
        };
        let called = self.call_provider(&target, CallRequest::RouteAddress(route), CALL_WITHIN);
        let router = &due.router;
        Ok(match called.await?.outcome {
            CallOutcome::AddressRouted => None,
            CallOutcome::Failed(f) => Some(format!("router {router}: {}: {}", f.reason, f.message)),
            _ => Some(format!("router {router}: an answer that answers nothing")),
        })
    }
}
