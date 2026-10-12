// SPDX-License-Identifier: AGPL-3.0-or-later

//! Approval: propose-only grants, approval scopes that only move up, and the rule that nobody
//! approves their own plan (§3.3 of docs/01-model.md, §4 of docs/08-identity.md).

use korpis_core::authz::evaluate::{Approval, may_approve};
use korpis_core::authz::grant::At;
use korpis_core::authz::{
    Action, AttenuationError, Decision, Delegation, Grant, Hierarchy, Scope, check_attenuation,
    evaluate,
};
use std::time::SystemTime;

use crate::support::{Tree, actions, child, request, root};

/// A grant to write Survival that only proposes: its plans await approval within Survival.
fn proposer(tree: &Tree, root: &Grant) -> Grant {
    let both = ["workload.write", "plan.approve"];
    let mut g = child(root, "user:junior", &both, Scope::Object(tree.survival));
    g.conditions.requires_approval_by = Some(Scope::Object(tree.survival));
    g.delegation = Delegation { allowed: true, max_depth: Some(2), actions: actions(&both) };
    g
}

/// Whether `approver`, holding `chains`, may approve a plan proposed through `proposing`.
fn approves(
    tree: &Tree,
    now: SystemTime,
    approver: &Grant,
    chains: &[Vec<Grant>],
    proposing: &[Grant],
) -> bool {
    let scope = Scope::Object(tree.survival);
    let decision = may_approve(
        &Approval {
            approver: &approver.subject,
            approver_chains: chains,
            proposer: &proposing[0].subject,
            proposing_chain: proposing,
            approval_scope: &scope,
            source_ip: None,
            mfa: false,
            now,
        },
        tree,
    );
    matches!(decision, Decision::Permit(_))
}

#[test]
fn a_propose_only_grant_proposes_and_its_approval_scope_only_moves_up() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let proposer = proposer(&tree, &root);

    let write = Action::parse("workload.write").unwrap();
    let w1 = tree.resource(tree.w1).unwrap();
    let decision = evaluate(
        &request(&proposer.subject, &write, &w1, now),
        &[vec![proposer.clone(), root.clone()]],
        &tree,
    );
    assert!(matches!(decision, Decision::Propose { .. }), "a propose-only grant proposes");

    let mut down = child(&proposer, "user:junior2", &["workload.write"], Scope::Object(tree.w1));
    down.conditions.requires_approval_by = Some(Scope::Object(tree.w1));
    assert_eq!(
        check_attenuation(&proposer, &down, &tree, At::Issue),
        Err(AttenuationError::ApprovalWeaker)
    );
    let mut up = down.clone();
    up.conditions.requires_approval_by = Some(Scope::Object(tree.customer));
    check_attenuation(&proposer, &up, &tree, At::Issue).unwrap();
}

#[test]
fn nobody_approves_their_own_plan_and_every_condition_binds_the_approver() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let p = [proposer(&tree, &root), root.clone()];
    let senior = child(&root, "user:senior", &["plan.approve"], Scope::Object(tree.customer));
    let held = |g: &Grant| vec![vec![g.clone(), root.clone()]];

    assert!(approves(&tree, now, &senior, &held(&senior), &p));
    let mut as_proposer = senior.clone();
    as_proposer.subject = p[0].subject.clone();
    assert!(
        !approves(&tree, now, &as_proposer, &held(&as_proposer), &p),
        "nobody approves their own plan"
    );
    let narrow = child(&root, "user:lead", &["plan.approve"], Scope::Object(tree.w1));
    assert!(
        !approves(&tree, now, &narrow, &held(&narrow), &p),
        "an approver's scope must contain the approval scope"
    );

    let mut needs_mfa = senior.clone();
    needs_mfa.conditions.requires_mfa = true;
    assert!(!approves(&tree, now, &needs_mfa, &held(&needs_mfa), &p));
    let mut gated = senior.clone();
    gated.conditions.requires_approval_by = Some(Scope::Object(tree.host));
    assert!(
        !approves(&tree, now, &gated, &held(&gated), &p),
        "an approval cannot itself await approval"
    );
    let mut spent = senior.clone();
    spent.conditions.max_uses = Some(1);
    spent.uses = 1;
    assert!(!approves(&tree, now, &spent, &held(&spent), &p));
}

/// A plan with an irreversible step needs approval even when its proposer's grant does not
/// (§3.3 of docs/01-model.md). Such a proposer could mint a sock puppet holding plan.approve over
/// the scope; everything about the puppet holds except the chain rule.
#[test]
fn a_chain_through_the_proposing_grant_is_not_a_second_approver() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let both = ["workload.delete", "plan.approve"];
    let mut operator = child(&root, "user:operator", &both, Scope::Object(tree.survival));
    operator.delegation =
        Delegation { allowed: true, max_depth: Some(1), actions: actions(&["plan.approve"]) };
    let puppet =
        child(&operator, "user:sock-puppet", &["plan.approve"], Scope::Object(tree.survival));
    check_attenuation(&operator, &puppet, &tree, At::Issue).unwrap();

    let proposing = [operator.clone(), root.clone()];
    let through_operator = [vec![puppet.clone(), operator.clone(), root.clone()]];
    assert!(!approves(&tree, now, &puppet, &through_operator, &proposing));
    let senior = child(&root, "user:senior", &["plan.approve"], Scope::Object(tree.customer));
    assert!(
        approves(&tree, now, &senior, &[vec![senior.clone(), root.clone()]], &proposing),
        "sharing the root with the proposer disqualifies nobody"
    );
}
