// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a member delegates (§5.1 of docs/08-identity.md): a member issues through their set
//! only when the child records the set and the member as `via`, and the child stands only while
//! the set's re-verification still confirms that member, so a member who left suspends what they
//! delegated without anyone revoking it.

use crate::support::{Tree, actions, child, root};
use korpis_core::authz::binding::ProviderRef;
use korpis_core::authz::evaluate::{ChainError, IssueError, issue, verify_chain};
use korpis_core::authz::scope::Vouched;
use korpis_core::authz::{Delegation, Grant, Scope, Subject, Via};
use korpis_core::principal::Principal;
use std::time::{Duration, SystemTime};

struct Setup {
    tree: Tree,
    chain: [Grant; 2],
    moderator: Subject,
    ann: Subject,
}

/// The `@moderator` set, vouched for, holding restart on Survival and delegating it once.
fn setup(now: SystemTime) -> Setup {
    let mut tree = Tree::new();
    let p = ProviderRef { organization: tree.reseller.id, name: "discord".into() };
    let set =
        Principal::ExternalSet { provider: p.key(), realm: "456".into(), member_of: "mod".into() };
    let ann = Principal::ExternalUser { provider: p.key(), user: "ann".into() };
    let root = root(&tree, now);
    let mut role = child(&root, "x", &["workload.restart"], Scope::Object(tree.survival));
    role.subject = set.subject();
    role.delegation =
        Delegation { allowed: true, max_depth: Some(1), actions: actions(&["workload.restart"]) };
    let vouched = Vouched {
        verified_until: Some(now + Duration::from_secs(60)),
        allows_irreversible: false,
        requires: Vec::new(),
    };
    tree.bindings.insert(role.subject.key.clone(), vouched);
    Setup { moderator: role.subject.clone(), chain: [role, root], tree, ann: ann.subject() }
}

fn helper(s: &Setup, via: Option<Via>) -> Grant {
    let mut g = child(&s.chain[0], "user:helper", &["workload.restart"], Scope::Object(s.tree.w1));
    g.via = via;
    g
}

#[test]
fn a_member_issues_through_their_set_only_recording_it() {
    let now = SystemTime::now();
    let s = setup(now);
    let via = Via { set: s.moderator.clone(), member: s.ann.clone() };
    let member = (&s.ann, std::slice::from_ref(&s.moderator));
    let recorded = helper(&s, Some(via.clone()));
    assert_eq!(issue(member, &s.chain, &recorded, false, &s.tree, now), Ok(()));
    let unrecorded = helper(&s, None);
    let refused = issue(member, &s.chain, &unrecorded, false, &s.tree, now);
    assert_eq!(refused, Err(IssueError::Via), "the child says through whom");
    let outsider = issue((&s.ann, &[]), &s.chain, &recorded, false, &s.tree, now);
    assert_eq!(outsider, Err(IssueError::NotHolder), "not asserted into the set");
}

#[test]
fn what_a_member_delegated_stands_only_while_the_member_is_confirmed() {
    let now = SystemTime::now();
    let mut s = setup(now);
    let via = Via { set: s.moderator.clone(), member: s.ann.clone() };
    let delegated = helper(&s, Some(via));
    let chain = [delegated.clone(), s.chain[0].clone(), s.chain[1].clone()];
    let unasked = verify_chain(&chain, &s.tree, now);
    assert_eq!(unasked, Err(ChainError::Unvouched(delegated.id)), "until the set confirms her");
    let key = (s.moderator.key.clone(), s.ann.key.clone());
    s.tree.members.insert(key.clone(), now + Duration::from_secs(60));
    assert_eq!(verify_chain(&chain, &s.tree, now), Ok(()));
    s.tree.members.insert(key, now - Duration::from_secs(1));
    let lapsed = verify_chain(&chain, &s.tree, now);
    assert_eq!(lapsed, Err(ChainError::Unvouched(delegated.id)), "she left, or nothing asked");
}
