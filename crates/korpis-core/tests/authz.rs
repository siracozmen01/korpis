// SPDX-License-Identifier: AGPL-3.0-or-later

//! The examples of docs/08-identity.md, run against the one authorization path.
//!
//! A three-level tenancy tree: a host, a reseller under it, a customer under the reseller. The
//! customer has project "Survival" with two workloads; W1 is labelled game=mc.

use korpis_core::authz::evaluate::{ChainError, may_approve};
use korpis_core::authz::grant::At;
use korpis_core::authz::scope::LabelSelector;
use korpis_core::authz::token::{self, Claims, KeyRing, SigningKey, TokenError};
use korpis_core::authz::{
    Action, AttenuationError, Conditions, Decision, Delegation, Grant, Hierarchy, Kind, Node,
    ObjectRef, Request, Resource, Scope, Subject, check_attenuation, evaluate,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

struct Tree {
    parents: HashMap<ObjectRef, ObjectRef>,
    labels: HashMap<ObjectRef, BTreeMap<String, String>>,
    host: ObjectRef,
    reseller: ObjectRef,
    customer: ObjectRef,
    survival: ObjectRef,
    w1: ObjectRef,
    w2: ObjectRef,
}

impl Tree {
    fn new() -> Self {
        let o = |k| ObjectRef::new(k, Uuid::now_v7());
        let (host, reseller, customer) =
            (o(Kind::Organization), o(Kind::Organization), o(Kind::Organization));
        let (survival, w1, w2) = (o(Kind::Project), o(Kind::Workload), o(Kind::Workload));
        let parents = HashMap::from([
            (reseller, host),
            (customer, reseller),
            (survival, customer),
            (w1, survival),
            (w2, survival),
        ]);
        let labels = HashMap::from([(w1, BTreeMap::from([("game".into(), "mc".into())]))]);
        Self { parents, labels, host, reseller, customer, survival, w1, w2 }
    }
}

impl Hierarchy for Tree {
    fn resource(&self, object: ObjectRef) -> Option<Resource> {
        let mut chain = Vec::new();
        let mut at = Some(object);
        while let Some(o) = at {
            chain
                .push(Node { object: o, labels: self.labels.get(&o).cloned().unwrap_or_default() });
            at = self.parents.get(&o).copied();
        }
        Some(Resource { chain })
    }
}

fn actions(names: &[&str]) -> BTreeSet<Action> {
    names.iter().map(|n| Action::parse(n).unwrap()).collect()
}

fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
}

/// The root grant created at installation: everything, unbounded delegation.
fn root(tree: &Tree, now: SystemTime) -> Grant {
    let all: Vec<&str> = korpis_core::authz::action::CORE.to_vec();
    let _ = now;
    Grant {
        id: Uuid::now_v7(),
        subject: Subject::new("user:host-admin"),
        actions: actions(&all),
        scope: Scope::Object(tree.host),
        conditions: Conditions::default(),
        parent: None,
        delegation: Delegation { allowed: true, max_depth: None, actions: actions(&all) },
        link_form: false,
        uses: 0,
        revoked: false,
        suspended: false,
    }
}

fn child(parent: &Grant, subject: &str, acts: &[&str], scope: Scope) -> Grant {
    Grant {
        id: Uuid::now_v7(),
        subject: Subject::new(subject),
        actions: actions(acts),
        scope,
        conditions: parent.conditions.clone(),
        parent: Some(parent.id),
        delegation: Delegation::default(),
        link_form: false,
        uses: 0,
        revoked: false,
        suspended: false,
    }
}

/// §3.2's moderator: workload.restart on Survival for 90 days, may mint one further level.
fn moderator(tree: &Tree, root: &Grant, now: SystemTime) -> Grant {
    let mut g = child(root, "discord:mod-1", &["workload.restart"], Scope::Object(tree.survival));
    g.conditions.expires_at = Some(now + days(90));
    g.delegation =
        Delegation { allowed: true, max_depth: Some(1), actions: actions(&["workload.restart"]) };
    g
}

/// §6.1's link: restart on W1 for 24 hours, three uses, no account, never delegates.
fn link(tree: &Tree, moderator: &Grant, now: SystemTime) -> Grant {
    let mut g = child(moderator, "link:abc", &["workload.restart"], Scope::Object(tree.w1));
    g.conditions.expires_at = Some(now + days(1));
    g.conditions.max_uses = Some(3);
    g.link_form = true;
    g
}

fn request<'a>(
    subject: &'a Subject,
    action: &'a Action,
    resource: &'a Resource,
    now: SystemTime,
) -> Request<'a> {
    Request { subject, action, resource, source_ip: None, mfa: false, now }
}

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
    let Decision::Permit { grant, redeem, propose_only, chain } = decision else {
        panic!("the link permits restarting W1: {decision:?}")
    };
    assert_eq!(grant, link.id);
    assert_eq!(chain, vec![link.id, moderator.id, root.id]);
    assert!(redeem, "a use-limited link is redeemed online (§6.2)");
    assert!(propose_only.is_none());

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
            Decision::Permit { .. }
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
        Decision::Permit { .. }
    ));
}

#[test]
fn approval_moves_up_the_tree_and_nobody_approves_their_own_plan() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let root = root(&tree, now);
    let mut proposer = child(
        &root,
        "user:junior",
        &["workload.write", "plan.approve"],
        Scope::Object(tree.survival),
    );
    proposer.conditions.requires_approval_by = Some(Scope::Object(tree.survival));
    proposer.delegation = Delegation {
        allowed: true,
        max_depth: Some(2),
        actions: actions(&["workload.write", "plan.approve"]),
    };

    let write = Action::parse("workload.write").unwrap();
    let w1 = tree.resource(tree.w1).unwrap();
    let decision = evaluate(
        &request(&proposer.subject, &write, &w1, now),
        &[vec![proposer.clone(), root.clone()]],
        &tree,
    );
    assert!(matches!(decision, Decision::Permit { propose_only: Some(_), .. }));

    let mut down = child(&proposer, "user:junior2", &["workload.write"], Scope::Object(tree.w1));
    down.conditions.requires_approval_by = Some(Scope::Object(tree.w1));
    assert_eq!(
        check_attenuation(&proposer, &down, &tree, At::Issue),
        Err(AttenuationError::ApprovalWeaker)
    );
    let mut up = down.clone();
    up.conditions.requires_approval_by = Some(Scope::Object(tree.customer));
    check_attenuation(&proposer, &up, &tree, At::Issue).unwrap();

    let senior = child(&root, "user:senior", &["plan.approve"], Scope::Object(tree.customer));
    let scope = Scope::Object(tree.survival);
    let p = [proposer.clone(), root.clone()];
    assert!(may_approve(
        &senior.subject,
        &[senior.clone(), root.clone()],
        &proposer.subject,
        &p,
        &scope,
        &tree,
        now
    ));
    assert!(
        !may_approve(
            &proposer.subject,
            &[senior.clone(), root.clone()],
            &proposer.subject,
            &p,
            &scope,
            &tree,
            now
        ),
        "nobody approves their own plan"
    );
    // A sock puppet holding plan.approve over the approval scope, minted from the proposing
    // grant: everything holds except the chain rule.
    let self_minted =
        child(&proposer, "user:sock-puppet", &["plan.approve"], Scope::Object(tree.survival));
    check_attenuation(&proposer, &self_minted, &tree, At::Issue).unwrap();
    assert!(
        !may_approve(
            &self_minted.subject,
            &[self_minted.clone(), proposer.clone(), root.clone()],
            &proposer.subject,
            &p,
            &scope,
            &tree,
            now
        ),
        "a chain through the proposing grant is not a second approver"
    );
    let narrow = child(&root, "user:lead", &["plan.approve"], Scope::Object(tree.w1));
    assert!(
        !may_approve(
            &narrow.subject,
            &[narrow.clone(), root],
            &proposer.subject,
            &p,
            &scope,
            &tree,
            now
        ),
        "an approver's scope must contain the approval scope"
    );
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
    assert!(matches!(evaluate(&r, &chains, &tree), Decision::Permit { .. }));
    r.source_ip = Some("192.168.1.1".parse().unwrap());
    assert!(matches!(evaluate(&r, &chains, &tree), Decision::Deny(_)));
    let mut r2 = request(&g.subject, &restart, &w2, now);
    r2.source_ip = Some("10.1.2.3".parse().unwrap());
    assert!(
        matches!(evaluate(&r2, &chains, &tree), Decision::Deny(_)),
        "W2 is not labelled game=mc"
    );
}

#[test]
fn tokens_verify_offline_and_refuse_everything_else() {
    let tree = Tree::new();
    let now = SystemTime::now();
    let key = SigningKey::generate(1);
    let mut keys = KeyRing::default();
    keys.insert(1, &key.public_bytes()).unwrap();
    let (leaf, parent, root) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let claims = Claims {
        subject: "user:alice".into(),
        chain: vec![leaf, parent, root],
        actions: vec!["workload.console.write".into()],
        scope: tree.w1,
        expires_at: now + token::SESSION_TOKEN_LIFETIME,
    };
    let t = token::mint(&key, &claims).unwrap();
    let w1 = tree.resource(tree.w1).unwrap();
    let none = HashSet::new();

    let ok = token::verify(&t, &keys, "workload.console.write", &w1, &none, now).unwrap();
    assert_eq!((ok.grant, ok.chain.len(), ok.subject.as_str()), (leaf, 3, "user:alice"));

    let refuse = |r: Result<_, TokenError>| assert!(r.is_err());
    refuse(token::verify(&t, &keys, "workload.exec", &w1, &none, now));
    refuse(token::verify(
        &t,
        &keys,
        "workload.console.write",
        &tree.resource(tree.w2).unwrap(),
        &none,
        now,
    ));
    refuse(token::verify(
        &t,
        &keys,
        "workload.console.write",
        &w1,
        &none,
        now + Duration::from_secs(121),
    ));
    assert_eq!(
        token::verify(&t, &keys, "workload.console.write", &w1, &HashSet::from([parent]), now),
        Err(TokenError::Revoked(parent)),
        "revoking any link of the chain refuses the token offline (§6.3)"
    );

    let shorter = token::restrict_expiry(&t, &keys, now + Duration::from_secs(10)).unwrap();
    token::verify(&shorter, &keys, "workload.console.write", &w1, &none, now).unwrap();
    refuse(token::verify(
        &shorter,
        &keys,
        "workload.console.write",
        &w1,
        &none,
        now + Duration::from_secs(11),
    ));

    // Restored: the key round-trips through its private bytes.
    let restored = SigningKey::from_bytes(1, &key.private_bytes()).unwrap();
    token::verify(
        &token::mint(&restored, &claims).unwrap(),
        &keys,
        "workload.console.write",
        &w1,
        &none,
        now,
    )
    .unwrap();

    keys.retire(1);
    refuse(token::verify(&t, &keys, "workload.console.write", &w1, &none, now));
}
