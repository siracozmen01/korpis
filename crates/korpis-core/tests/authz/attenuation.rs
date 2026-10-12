// SPDX-License-Identifier: AGPL-3.0-or-later

//! Attenuation: every dimension of a child is checked against its parent (§3 of
//! docs/08-identity.md).

use korpis_core::authz::evaluate::ChainError;
use korpis_core::authz::grant::At;
use korpis_core::authz::{
    Action, AttenuationError, Decision, Delegation, Hierarchy, Kind, ObjectRef, Permit, Scope,
    Subject, check_attenuation, evaluate,
};
use std::collections::BTreeSet;
use std::time::SystemTime;
use uuid::Uuid;

use crate::support::{Tree, actions, child, days, link, moderator, request, root};

#[test]
fn the_moderator_mints_a_link_and_the_link_mints_nothing() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let moderator = moderator(&tree, &root, now);
    let link = link(&tree, &moderator, now);

    check_attenuation(&root, &moderator, &tree, At::Issue).unwrap();
    check_attenuation(&moderator, &link, &tree, At::Issue).unwrap();

    let restart = Action::parse("workload.restart").unwrap();
    let w1 = tree.resource(tree.w1).unwrap();
    let decision = evaluate(
        &request(&link.subject, &restart, &w1, now),
        &[vec![link.clone(), moderator.clone(), root.clone()]],
        &tree,
    );
    let Decision::Permit(Permit { grant, redeem, chain }) = decision else {
        panic!("the link permits restarting W1: {decision:?}")
    };
    assert_eq!(grant, link.id);
    assert_eq!(chain, vec![link.id, moderator.id, root.id]);
    assert!(redeem, "a use-limited link is redeemed online (§6.2)");

    let mut grandchild =
        child(&link, "someone-else", &["workload.restart"], Scope::Object(tree.w1));
    grandchild.conditions.max_uses = Some(1);
    assert_eq!(
        check_attenuation(&link, &grandchild, &tree, At::Issue),
        Err(AttenuationError::DelegationNotAllowed),
        "a link-form grant can never delegate"
    );
}

#[test]
fn every_dimension_of_attenuation_is_checked() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let moderator = moderator(&tree, &root, now);

    let mut c = link(&tree, &moderator, now);
    c.conditions.expires_at = Some(now + days(91));
    assert_eq!(
        check_attenuation(&moderator, &c, &tree, At::Issue),
        Err(AttenuationError::ExpiryWider)
    );

    let mut c = link(&tree, &moderator, now);
    c.actions = actions(&["workload.restart", "workload.stop"]);
    assert!(matches!(
        check_attenuation(&moderator, &c, &tree, At::Issue),
        Err(AttenuationError::ActionNotHeld(_))
    ));

    let other_project = ObjectRef::new(Kind::Project, Uuid::now_v7());
    let mut c = link(&tree, &moderator, now);
    c.scope = Scope::Object(other_project);
    assert_eq!(
        check_attenuation(&moderator, &c, &tree, At::Issue),
        Err(AttenuationError::ScopeNotContained)
    );

    let mut c = link(&tree, &moderator, now);
    c.link_form = false;
    c.delegation = Delegation { allowed: true, max_depth: Some(0), actions: BTreeSet::new() };
    assert_eq!(
        check_attenuation(&moderator, &c, &tree, At::Issue),
        Err(AttenuationError::DelegationWider),
        "max_depth 1 permits a child but not a delegating child"
    );

    let mut c = link(&tree, &moderator, now);
    c.delegation = Delegation { allowed: true, max_depth: Some(0), actions: BTreeSet::new() };
    assert_eq!(
        check_attenuation(&root, &c, &tree, At::Issue),
        Err(AttenuationError::LinkFormDelegates)
    );
}

#[test]
fn use_limits_are_taken_from_the_parents_remaining_uses() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let mut moderator = moderator(&tree, &root, now);
    moderator.conditions.max_uses = Some(10);
    moderator.uses = 7;

    let mut c = link(&tree, &moderator, now);
    c.conditions.max_uses = Some(5);
    assert_eq!(
        check_attenuation(&moderator, &c, &tree, At::Issue),
        Err(AttenuationError::UsesWider)
    );
    c.conditions.max_uses = None;
    assert_eq!(
        check_attenuation(&moderator, &c, &tree, At::Issue),
        Err(AttenuationError::UsesWider)
    );
    c.conditions.max_uses = Some(3);
    check_attenuation(&moderator, &c, &tree, At::Issue).unwrap();
    // At use, the parent's remaining uses have fallen by what its children took; not a failure.
    moderator.uses = 10;
    check_attenuation(&moderator, &c, &tree, At::Use).unwrap();
}

#[test]
fn revoking_an_ancestor_refuses_the_whole_subtree_at_use() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let mut moderator = moderator(&tree, &root, now);
    let link = link(&tree, &moderator, now);
    moderator.revoked = true;

    let restart = Action::parse("workload.restart").unwrap();
    let w1 = tree.resource(tree.w1).unwrap();
    let decision = evaluate(
        &request(&link.subject, &restart, &w1, now),
        &[vec![link.clone(), moderator.clone(), root]],
        &tree,
    );
    let Decision::Deny(denial) = decision else { panic!("revocation is derived from the chain") };
    assert_eq!(denial.reasons[0].1, ChainError::Revoked(moderator.id));
}

#[test]
fn irreversible_actions_stay_out_of_links_and_chat_roles() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let mut link = child(&root, "link:x", &["workload.delete"], Scope::Object(tree.w1));
    link.link_form = true;
    assert!(matches!(
        check_attenuation(&root, &link, &tree, At::Issue),
        Err(AttenuationError::IrreversibleToBearerOrSet(_))
    ));
    link.irreversible_override = true;
    assert!(
        check_attenuation(&root, &link, &tree, At::Issue).is_err(),
        "a link is never overridable"
    );

    let mut role =
        child(&root, "x", &["workload.delete", "workload.restart"], Scope::Object(tree.survival));
    role.subject = Subject::set("discord:guild-456:role-mod");
    assert!(check_attenuation(&root, &role, &tree, At::Issue).is_err());
    role.irreversible_override = true;
    check_attenuation(&root, &role, &tree, At::Issue).expect("an operator may override a binding");
    role.irreversible_override = false;
    role.actions = actions(&["workload.restart"]);
    check_attenuation(&root, &role, &tree, At::Issue).unwrap();
}
