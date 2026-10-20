// SPDX-License-Identifier: AGPL-3.0-or-later

//! Acting as a member of a set (§5.2 of docs/08-identity.md): a person acting through a relay
//! holds what the sets the assertion names hold, and through a set whose binding requires its
//! members to be in another, only while the assertion names that one too.

use crate::support::{Tree, child, request, root};
use korpis_core::authz::binding::ProviderRef;
use korpis_core::authz::evaluate::ChainError;
use korpis_core::authz::scope::Vouched;
use korpis_core::authz::{Action, Decision, Grant, Hierarchy, Scope, Subject, evaluate};
use korpis_core::principal::Principal;
use std::time::{Duration, SystemTime};

fn group(p: &ProviderRef, name: &str) -> Subject {
    let set =
        Principal::ExternalSet { provider: p.key(), realm: "456".into(), member_of: name.into() };
    set.subject()
}

/// The `@moderator` set holding restart on Survival, its binding vouching and requiring its
/// members to be in `requires` too.
fn moderators(tree: &mut Tree, now: SystemTime, requires: &[&Subject]) -> [Grant; 2] {
    let p = ProviderRef { organization: tree.reseller.id, name: "discord".into() };
    let root = root(tree, now);
    let mut role = child(&root, "x", &["workload.restart"], Scope::Object(tree.survival));
    role.subject = group(&p, "moderator");
    let vouched = Vouched {
        verified_until: Some(now + Duration::from_secs(60)),
        allows_irreversible: false,
        requires: requires.iter().map(|s| s.key.clone()).collect(),
    };
    tree.bindings.insert(role.subject.key.clone(), vouched);
    [role, root]
}

fn decided(tree: &Tree, chain: &[Grant], memberships: &[Subject], now: SystemTime) -> Decision {
    let p = ProviderRef { organization: tree.reseller.id, name: "discord".into() };
    let ann = Principal::ExternalUser { provider: p.key(), user: "ann".into() }.subject();
    let action = Action::parse("workload.restart").unwrap();
    let resource = tree.resource(tree.w1).unwrap();
    let mut asked = request(&ann, &action, &resource, now);
    asked.memberships = memberships;
    evaluate(&asked, &[chain.to_vec()], tree)
}

fn reasons(d: &Decision) -> Vec<ChainError> {
    match d {
        Decision::Deny(denial) => denial.reasons.iter().map(|(_, e)| e.clone()).collect(),
        other => panic!("permitted: {other:?}"),
    }
}

#[test]
fn a_member_holds_what_their_set_holds_only_while_the_assertion_names_it() {
    let mut tree = Tree::new();
    let now = SystemTime::now();
    let chain = moderators(&mut tree, now, &[]);
    let p = ProviderRef { organization: tree.reseller.id, name: "discord".into() };
    let moderator = group(&p, "moderator");
    assert!(matches!(decided(&tree, &chain, &[moderator], now), Decision::Permit(_)));
    let other = group(&p, "helper");
    assert_eq!(reasons(&decided(&tree, &chain, &[other], now)), [ChainError::WrongSubject]);
    assert_eq!(reasons(&decided(&tree, &chain, &[], now)), [ChainError::WrongSubject]);
}

#[test]
fn a_set_s_required_membership_is_held_to_the_assertion_at_use() {
    let mut tree = Tree::new();
    let now = SystemTime::now();
    let p = ProviderRef { organization: tree.reseller.id, name: "discord".into() };
    let verified = group(&p, "verified");
    let chain = moderators(&mut tree, now, &[&verified]);
    let moderator = group(&p, "moderator");
    let without = decided(&tree, &chain, std::slice::from_ref(&moderator), now);
    assert_eq!(reasons(&without), [ChainError::MembershipRequired]);
    let with = decided(&tree, &chain, &[moderator, verified], now);
    assert!(matches!(with, Decision::Permit(_)), "{with:?}");
}
