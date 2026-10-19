// SPDX-License-Identifier: AGPL-3.0-or-later

//! An organization's suspension, derived at use (§2.1 and §3.5 of docs/08-identity.md): a grant
//! scoped in a suspended organization's subtree stops authorizing everything but the reads a
//! suspended tenant keeps, every grant beneath it with it, while a grant scoped above the
//! suspended organization still acts on it; resuming restores them all unchanged, since nothing
//! was written onto any of them.

use korpis_core::authz::evaluate::ChainError;
use korpis_core::authz::{Action, Decision, Delegation, Hierarchy, Scope, evaluate};
use std::time::SystemTime;

use crate::support::{Tree, actions, child, request, root};

fn denied_for(decision: &Decision) -> Option<ChainError> {
    match decision {
        Decision::Deny(d) => d.reasons.first().map(|(_, e)| e.clone()),
        _ => None,
    }
}

#[test]
fn a_suspended_reseller_suspends_its_customers_grants_and_not_the_hosts() {
    let mut tree = Tree::new();
    let now = SystemTime::now();
    let host = root(&tree, now);
    let acts = ["workload.restart", "workload.read", "workload.console.read"];
    let mut reseller = child(&host, "user:reseller", &acts, Scope::Object(tree.reseller));
    reseller.delegation = Delegation { allowed: true, max_depth: Some(1), actions: actions(&acts) };
    let customer = child(&reseller, "user:customer", &acts, Scope::Object(tree.survival));
    let w1 = tree.resource(tree.w1).unwrap();
    let restart = Action::parse("workload.restart").unwrap();
    let read = Action::parse("workload.read").unwrap();
    let console = Action::parse("workload.console.read").unwrap();
    let customer_chain = [vec![customer.clone(), reseller.clone(), host.clone()]];
    let as_customer = |tree: &Tree, action: &Action| {
        let w1 = tree.resource(tree.w1).unwrap();
        evaluate(&request(&customer.subject, action, &w1, now), &customer_chain, tree)
    };
    assert!(matches!(as_customer(&tree, &restart), Decision::Permit(_)), "before");

    tree.suspended.insert(tree.reseller);
    assert_eq!(denied_for(&as_customer(&tree, &restart)), Some(ChainError::OrganizationSuspended));
    assert_eq!(
        denied_for(&as_customer(&tree, &console)),
        Some(ChainError::OrganizationSuspended),
        "its console is the service suspension withholds"
    );
    assert!(matches!(as_customer(&tree, &read), Decision::Permit(_)), "its status stays readable");
    let host_chain = [vec![host.clone()]];
    let by_host = evaluate(&request(&host.subject, &restart, &w1, now), &host_chain, &tree);
    assert!(matches!(by_host, Decision::Permit(_)), "the host, above it, still acts on it");

    tree.suspended.clear();
    assert!(matches!(as_customer(&tree, &restart), Decision::Permit(_)), "resumed, unchanged");
}

#[test]
fn a_suspended_customer_leaves_its_reseller_untouched() {
    let mut tree = Tree::new();
    let now = SystemTime::now();
    let host = root(&tree, now);
    let reseller =
        child(&host, "user:reseller", &["workload.restart"], Scope::Object(tree.reseller));
    tree.suspended.insert(tree.customer);
    let w1 = tree.resource(tree.w1).unwrap();
    let restart = Action::parse("workload.restart").unwrap();
    let chain = [vec![reseller.clone(), host]];
    let decision = evaluate(&request(&reseller.subject, &restart, &w1, now), &chain, &tree);
    assert!(matches!(decision, Decision::Permit(_)), "suspension is effective from above");
}
