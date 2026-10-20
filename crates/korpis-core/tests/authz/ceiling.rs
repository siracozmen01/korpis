// SPDX-License-Identifier: AGPL-3.0-or-later

//! The delegation ceiling (§5.1 of docs/08-identity.md): a grant beneath a set subject stands
//! for at most the ceiling from its issue, whatever its own expiry, at every use; an
//! organization's lowered ceiling reaches grants already issued in its subtree, and the root's
//! raised one lets them stand longer. The set's own grant is not beneath a set and is not held to
//! it.

use crate::support::{Tree, actions, child, days, root};
use korpis_core::authz::binding::{DELEGATION_CEILING, ProviderRef};
use korpis_core::authz::evaluate::{ChainError, verify_chain};
use korpis_core::authz::scope::Vouched;
use korpis_core::authz::{Delegation, Grant, Scope};
use korpis_core::principal::Principal;
use std::time::{Duration, SystemTime};

/// The `@moderator` set holding restart on Survival, issued `role_age` ago and vouched for, and
/// a helper's grant beneath it issued `helper_age` ago with no expiry of its own.
fn chain(tree: &mut Tree, now: SystemTime, role_age: Duration, helper_age: Duration) -> [Grant; 3] {
    let root = root(tree, now - days(400));
    let mut role = child(&root, "x", &["workload.restart"], Scope::Object(tree.survival));
    let p = ProviderRef { organization: tree.reseller.id, name: "discord".into() };
    let set =
        Principal::ExternalSet { provider: p.key(), realm: "456".into(), member_of: "mod".into() };
    role.subject = set.subject();
    role.issued_at = now - role_age;
    role.delegation =
        Delegation { allowed: true, max_depth: Some(1), actions: actions(&["workload.restart"]) };
    let vouched = Vouched {
        verified_until: Some(now + Duration::from_secs(60)),
        allows_irreversible: false,
        requires: Vec::new(),
    };
    tree.bindings.insert(role.subject.key.clone(), vouched);
    let mut helper = child(&role, "user:helper", &["workload.restart"], Scope::Object(tree.w1));
    helper.issued_at = now - helper_age;
    [helper, role, root]
}

#[test]
fn a_grant_beneath_a_set_lasts_the_ceiling_from_its_issue_and_the_set_s_own_does_not() {
    let mut tree = Tree::new();
    let now = SystemTime::now();
    let [helper, role, root] = chain(&mut tree, now, days(90), days(6));
    assert_eq!(DELEGATION_CEILING, days(7));
    let fresh = [helper.clone(), role.clone(), root.clone()];
    assert_eq!(verify_chain(&fresh, &tree, now), Ok(()), "six days old, under seven");
    assert_eq!(verify_chain(&[role.clone(), root.clone()], &tree, now), Ok(()), "the set's own");
    let [old, ..] = chain(&mut tree, now, days(90), days(8));
    let stale = [old.clone(), role, root];
    assert_eq!(verify_chain(&stale, &tree, now), Err(ChainError::NotInForce(old.id)));
}

#[test]
fn a_lowered_ceiling_reaches_grants_issued_already_and_a_raised_root_lets_them_stand() {
    let mut tree = Tree::new();
    let now = SystemTime::now();
    let [helper, role, root] = chain(&mut tree, now, days(90), days(6));
    let links = [helper.clone(), role, root];
    tree.ceilings.insert(tree.customer, days(1));
    assert_eq!(verify_chain(&links, &tree, now), Err(ChainError::NotInForce(helper.id)));
    tree.ceilings.insert(tree.host, days(30));
    assert_eq!(
        verify_chain(&links, &tree, now),
        Err(ChainError::NotInForce(helper.id)),
        "the lowest along the organizations is in force"
    );
    tree.ceilings.remove(&tree.customer);
    let [older, role, root] = chain(&mut tree, now, days(90), days(20));
    assert_eq!(verify_chain(&[older, role, root], &tree, now), Ok(()), "the root raised it");
}
