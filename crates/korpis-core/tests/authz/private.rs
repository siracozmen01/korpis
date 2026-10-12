// SPDX-License-Identifier: AGPL-3.0-or-later

//! Private actions stop at the organization boundary unless granted explicitly (§7 of
//! docs/08-identity.md).

use korpis_core::authz::evaluate::ChainError;
use korpis_core::authz::grant::At;
use korpis_core::authz::scope::LabelSelector;
use korpis_core::authz::{Action, Decision, Hierarchy, Scope, check_attenuation, evaluate};
use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::support::{Tree, child, request, root};

#[test]
fn private_actions_do_not_cross_into_a_customer_implicitly() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let w1 = tree.resource(tree.w1).unwrap();

    let read_files = Action::parse("workload.file.read").unwrap();
    let stop = Action::parse("workload.stop").unwrap();
    let chains = [vec![root.clone()]];
    let refused = evaluate(&request(&root.subject, &read_files, &w1, now), &chains, &tree);
    let Decision::Deny(denial) = refused else { panic!("the host's root grant reaches no files") };
    assert_eq!(denial.reasons[0].1, ChainError::PrivateAcrossOrganization);
    assert!(
        matches!(
            evaluate(&request(&root.subject, &stop, &w1, now), &chains, &tree),
            Decision::Permit(_)
        ),
        "operating the capacity it sells flows down as before"
    );

    // The reseller, one level above the customer, is a strict ancestor too.
    let reseller =
        child(&root, "user:reseller", &["workload.file.read"], Scope::Object(tree.reseller));
    assert!(matches!(
        evaluate(
            &request(&reseller.subject, &read_files, &w1, now),
            &[vec![reseller.clone(), root.clone()]],
            &tree
        ),
        Decision::Deny(_)
    ));

    // The explicit grant of §7: the host mints itself a grant scoped to the customer.
    let support =
        child(&root, "user:host-admin", &["workload.file.read"], Scope::Object(tree.customer));
    check_attenuation(&root, &support, &tree, At::Issue).unwrap();
    assert!(matches!(
        evaluate(
            &request(&root.subject, &read_files, &w1, now),
            &[vec![support, root.clone()]],
            &tree
        ),
        Decision::Permit(_)
    ));
}

#[test]
fn selector_scopes_follow_labels_and_networks_bind() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let selector = LabelSelector {
        match_labels: BTreeMap::from([("game".into(), "mc".into())]),
        expressions: vec![],
    };
    let mut g = child(
        &root,
        "user:mc-ops",
        &["workload.restart"],
        Scope::Selector { within: tree.survival, selector },
    );
    g.conditions.source_cidr = vec!["10.0.0.0/8".parse().unwrap()];
    check_attenuation(&root, &g, &tree, At::Issue).unwrap();

    let restart = Action::parse("workload.restart").unwrap();
    let (w1, w2) = (tree.resource(tree.w1).unwrap(), tree.resource(tree.w2).unwrap());
    let chains = [vec![g.clone(), root]];
    let mut r = request(&g.subject, &restart, &w1, now);
    r.source_ip = Some("10.1.2.3".parse().unwrap());
    assert!(matches!(evaluate(&r, &chains, &tree), Decision::Permit(_)));
    r.source_ip = Some("192.168.1.1".parse().unwrap());
    assert!(matches!(evaluate(&r, &chains, &tree), Decision::Deny(_)));
    let mut r2 = request(&g.subject, &restart, &w2, now);
    r2.source_ip = Some("10.1.2.3".parse().unwrap());
    assert!(
        matches!(evaluate(&r2, &chains, &tree), Decision::Deny(_)),
        "W2 is not labelled game=mc"
    );
}
