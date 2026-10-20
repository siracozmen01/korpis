// SPDX-License-Identifier: AGPL-3.0-or-later

//! Issuing: only a holder of a valid parent attenuates it, and roots come from installation
//! (§3.4 and §9.1 of docs/08-identity.md).

use korpis_core::authz::evaluate::{ChainError, IssueError, issue};
use korpis_core::authz::{Action, Decision, Hierarchy, Scope, Subject, evaluate};
use std::time::SystemTime;
use uuid::Uuid;

use crate::support::{Tree, child, link, moderator, request, root};

#[test]
fn only_the_holder_of_a_valid_parent_can_issue() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let mut moderator = moderator(&tree, &root, now);
    let link = link(&tree, &moderator, now);
    let chain = [moderator.clone(), root.clone()];

    issue((&moderator.subject, &[]), &chain, &link, false, &tree, now).unwrap();
    assert_eq!(
        issue((&Subject::new("user:mallory"), &[]), &chain, &link, false, &tree, now),
        Err(IssueError::NotHolder),
        "issuing is attenuating a grant one holds"
    );
    moderator.revoked = true;
    assert!(matches!(
        issue(
            (&moderator.subject, &[]),
            &[moderator.clone(), root.clone()],
            &link,
            false,
            &tree,
            now
        ),
        Err(IssueError::ParentChain(ChainError::Revoked(_)))
    ));
}

#[test]
fn the_override_comes_from_the_binding_and_roots_from_installation() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let chain = [root.clone()];
    let mut role = child(&root, "x", &["workload.delete"], Scope::Object(tree.survival));
    role.subject = Subject::set("discord:guild-456:role-mod");
    role.irreversible_override = true;
    assert_eq!(
        issue((&root.subject, &[]), &chain, &role, false, &tree, now),
        Err(IssueError::OverrideNotFromBinding),
        "a request cannot set the override itself"
    );
    issue((&root.subject, &[]), &chain, &role, true, &tree, now).expect("the binding allows it");

    let mut individual = role.clone();
    individual.subject = Subject::new("user:bob");
    assert_eq!(
        issue((&root.subject, &[]), &chain, &individual, true, &tree, now),
        Err(IssueError::OverrideNotFromBinding),
        "the override exists only for set subjects"
    );

    let mut fake_root = root.clone();
    fake_root.id = Uuid::now_v7();
    fake_root.root = false;
    let restart = Action::parse("workload.restart").unwrap();
    let w1 = tree.resource(tree.w1).unwrap();
    let Decision::Deny(denial) = evaluate(
        &request(&fake_root.subject, &restart, &w1, now),
        &[vec![fake_root.clone()]],
        &tree,
    ) else {
        panic!("a parentless grant that is not a root roots nothing");
    };
    assert_eq!(denial.reasons[0].1, ChainError::NotRooted(fake_root.id));

    let mut minted_root = child(&root, "user:eve", &["workload.restart"], Scope::Object(tree.w1));
    minted_root.root = true;
    assert_eq!(
        issue((&root.subject, &[]), &chain, &minted_root, false, &tree, now),
        Err(IssueError::RootNotIssuable)
    );
}
