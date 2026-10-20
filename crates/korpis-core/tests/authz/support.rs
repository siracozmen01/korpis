// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tenancy tree of the examples, and the grants §3.2 and §6.1 of docs/08-identity.md build.

use korpis_core::authz::scope::Vouched;
use korpis_core::authz::{
    Action, Conditions, Delegation, Grant, Hierarchy, Kind, Node, ObjectRef, Request, Resource,
    Scope, Subject,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

pub struct Tree {
    pub parents: HashMap<ObjectRef, ObjectRef>,
    pub labels: HashMap<ObjectRef, BTreeMap<String, String>>,
    /// The organizations suspended (§2.1 of docs/08-identity.md).
    pub suspended: BTreeSet<ObjectRef>,
    /// The live bindings of external subjects, by key (§5 of docs/08-identity.md).
    pub bindings: HashMap<String, Vouched>,
    /// The delegation ceilings organizations set (§5.1 of docs/08-identity.md).
    pub ceilings: HashMap<ObjectRef, Duration>,
    /// Until when each (set, member) membership was last confirmed to hold (§5.1).
    pub members: HashMap<(String, String), SystemTime>,
    pub host: ObjectRef,
    pub reseller: ObjectRef,
    pub customer: ObjectRef,
    pub survival: ObjectRef,
    pub w1: ObjectRef,
    pub w2: ObjectRef,
}

impl Tree {
    pub fn new() -> Self {
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
        Self {
            parents,
            labels,
            suspended: BTreeSet::new(),
            bindings: HashMap::new(),
            ceilings: HashMap::new(),
            members: HashMap::new(),
            host,
            reseller,
            customer,
            survival,
            w1,
            w2,
        }
    }
}

impl Hierarchy for Tree {
    fn resource(&self, object: ObjectRef) -> Option<Resource> {
        let mut chain = Vec::new();
        let mut at = Some(object);
        while let Some(o) = at {
            chain.push(Node {
                object: o,
                labels: self.labels.get(&o).cloned().unwrap_or_default(),
                suspended: self.suspended.contains(&o),
                ceiling: self.ceilings.get(&o).copied(),
            });
            at = self.parents.get(&o).copied();
        }
        Some(Resource { chain })
    }

    fn vouched(&self, subject: &str) -> Option<Vouched> {
        self.bindings.get(subject).cloned()
    }

    fn member_vouched(&self, set: &str, member: &str) -> Option<SystemTime> {
        self.members.get(&(set.to_owned(), member.to_owned())).copied()
    }
}

pub fn actions(names: &[&str]) -> BTreeSet<Action> {
    names.iter().map(|n| Action::parse(n).unwrap()).collect()
}

pub fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
}

/// The root grant created at installation: everything, unbounded delegation.
pub fn root(tree: &Tree, now: SystemTime) -> Grant {
    let all: Vec<&str> = korpis_core::authz::action::CORE.to_vec();
    Grant {
        id: Uuid::now_v7(),
        issued_at: now,
        via: None,
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
        irreversible_override: false,
        root: true,
    }
}

pub fn child(parent: &Grant, subject: &str, acts: &[&str], scope: Scope) -> Grant {
    Grant {
        id: Uuid::now_v7(),
        issued_at: parent.issued_at,
        via: None,
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
        irreversible_override: false,
        root: false,
    }
}

/// §3.2's moderator: workload.restart on Survival for 90 days, may mint one further level.
pub fn moderator(tree: &Tree, root: &Grant, now: SystemTime) -> Grant {
    let mut g = child(root, "discord:mod-1", &["workload.restart"], Scope::Object(tree.survival));
    g.conditions.expires_at = Some(now + days(90));
    g.delegation =
        Delegation { allowed: true, max_depth: Some(1), actions: actions(&["workload.restart"]) };
    g
}

/// §6.1's link: restart on W1 for 24 hours, three uses, no account, never delegates.
pub fn link(tree: &Tree, moderator: &Grant, now: SystemTime) -> Grant {
    let mut g = child(moderator, "link:abc", &["workload.restart"], Scope::Object(tree.w1));
    g.conditions.expires_at = Some(now + days(1));
    g.conditions.max_uses = Some(3);
    g.link_form = true;
    g
}

pub fn request<'a>(
    subject: &'a Subject,
    action: &'a Action,
    resource: &'a Resource,
    now: SystemTime,
) -> Request<'a> {
    let memberships = &[];
    Request {
        subject,
        memberships,
        action,
        resource,
        source_ip: None,
        mfa: false,
        now,
        renewal: false,
    }
}
