// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::labels::{LabelSelector, Labels};
use crate::node::Taint;
use crate::node::driver::{FileAccess, IdMapCaps, VolumeSemantics};
use crate::node::{Capabilities, Connectivity, DriverInfo, Enforcement, NodeAdmin, Reachability};
use crate::recipe::RecipeRef;
use crate::tenancy::Name;
use crate::value::ValueMap;
use crate::workload::spec::placement::{AffinityRule, Toleration};
use crate::workload::spec::{
    ConsoleMode, Continuity, HealthSpec, IsolationTier, Lifecycle, PlacementSpec, ResourceSpec,
    RuntimeSpec,
};
use std::time::SystemTime;

const GIB: u64 = 1 << 30;
const ORG: Uuid = Uuid::from_u128(7);
const PROJECT: Uuid = Uuid::from_u128(8);

fn driver(enforced: &[&str]) -> DriverInfo {
    let enforcement = enforced
        .iter()
        .map(|k| {
            let e = Enforcement {
                enforced: true,
                mechanism: "cgroup v2".into(),
                granularity: String::new(),
            };
            ((*k).to_owned(), e)
        })
        .collect();
    DriverInfo {
        name: "oci".into(),
        version: "1".into(),
        protocol: "1".into(),
        healthy: true,
        capabilities: Capabilities {
            tiers: vec![IsolationTier::Container],
            console: vec![ConsoleMode::Logs, ConsoleMode::Interactive],
            exec: true,
            signals: Vec::new(),
            hot_resize: Vec::new(),
            hot_volume_attach: false,
            runtime_snapshot: None,
            live_migrate: false,
            gpu: Vec::new(),
            devices: Vec::new(),
            rootfs_sources: Vec::new(),
            network_modes: Vec::new(),
            enforcement,
            volume_semantics: VolumeSemantics::Filesystem,
            file_access: FileAccess::HostWorker,
            id_mapping: IdMapCaps::default(),
            config_schema: String::new(),
            disruptive: vec![],
        },
    }
}

fn node(n: u128, labels: &[(&str, &str)]) -> Node {
    let cap = Bounds::from([(Dimension::Cpu, 4_000), (Dimension::Memory, 8 * GIB)]);
    Node {
        id: Uuid::from_u128(n),
        name: Name::parse(format!("n{n}")).unwrap(),
        organization: ORG,
        labels: Labels::new(labels.iter().map(|(k, v)| ((*k).into(), (*v).into()))).unwrap(),
        taints: Vec::new(),
        capacity: cap.clone(),
        allocatable: cap,
        drivers: vec![driver(&["memory", "cpu"])],
        storage_classes: Vec::new(),
        reachability: Reachability::Public,
        advertised: None,
        probed_at: None,
        admin: NodeAdmin::Active,
        connectivity: Connectivity::Connected,
        agent_version: None,
        last_seen: None,
        enrolled_at: SystemTime::UNIX_EPOCH,
        version: 1,
    }
}

fn body(memory: u64) -> IntentBody {
    let mut resources = ResourceSpec::default();
    resources.memory.reservation = memory;
    resources.cpu.reservation_millis = 1_000;
    IntentBody {
        recipe: RecipeRef {
            name: "r".into(),
            version: "1".into(),
            digest: String::new(),
            trust: None,
            providers: Vec::new(),
        },
        lifecycle: Lifecycle::Persistent,
        schedule: None,
        // No stream, so these cases reserve only what they set: a stream reserves node disk.
        console: ConsoleMode::None,
        runtime: RuntimeSpec {
            driver: "oci".into(),
            tier: IsolationTier::Container,
            config: ValueMap::new(),
        },
        resources,
        volumes: Vec::new(),
        endpoints: Vec::new(),
        health: HealthSpec::default(),
        config: ValueMap::new(),
        placement: PlacementSpec::default(),
        continuity: Continuity::Derived,
        dependencies: Vec::new(),
        streams: crate::workload::spec::StreamSpec::default(),
    }
}

struct Case {
    body: IntentBody,
    labels: BTreeMap<String, String>,
    nodes: Vec<Node>,
    allocated: BTreeMap<Uuid, Bounds>,
    peers: Vec<Peer>,
    policy: PlacementPolicy,
}

impl Case {
    fn new(nodes: Vec<Node>) -> Self {
        Self {
            body: body(GIB),
            labels: BTreeMap::new(),
            nodes,
            allocated: BTreeMap::new(),
            peers: Vec::new(),
            policy: PlacementPolicy::default(),
        }
    }

    fn run(&self) -> Explanation {
        let empty = Bounds::new();
        let candidates: Vec<_> = self
            .nodes
            .iter()
            .map(|n| Candidate { node: n, allocated: self.allocated.get(&n.id).unwrap_or(&empty) })
            .collect();
        let req = Request {
            workload: Uuid::from_u128(1000),
            project: PROJECT,
            labels: &self.labels,
            owners: &[ORG],
            body: &self.body,
            current: None,
            edge_tier: false,
            volumes: &[],
        };
        place(&req, &candidates, &self.peers, &self.policy)
    }
}

fn checks(e: &Explanation, n: u128) -> Vec<Check> {
    e.filtered.iter().filter(|r| r.node == Uuid::from_u128(n)).map(|r| r.check).collect()
}

#[test]
fn every_rejection_is_recorded_with_its_check() {
    let mut cordoned = node(2, &[]);
    cordoned.admin = NodeAdmin::Cordoned;
    let mut vm_only = node(3, &[]);
    vm_only.drivers[0].capabilities.tiers = vec![IsolationTier::Vm];
    let mut foreign = node(4, &[]);
    foreign.organization = Uuid::from_u128(99);
    let mut tainted = node(5, &[]);
    tainted.taints = vec![Taint { key: "gpu".into(), value: "only".into() }];
    let mut case = Case::new(vec![node(1, &[]), cordoned, vm_only, foreign, tainted]);
    case.allocated.insert(Uuid::from_u128(1), Bounds::from([(Dimension::Memory, 8 * GIB)]));
    let e = case.run();
    assert_eq!(e.candidates, 5);
    assert_eq!(checks(&e, 1), [Check::Capacity]);
    assert_eq!(checks(&e, 2), [Check::Status]);
    assert_eq!(checks(&e, 3), [Check::Tier]);
    assert_eq!(checks(&e, 4), [Check::Owner]);
    assert_eq!(checks(&e, 5), [Check::Taint]);
    assert_eq!(e.chosen, None, "nothing survived: the workload is unplaced, with every reason");
    case.body.placement.tolerations = vec![Toleration { key: "gpu".into(), value: String::new() }];
    assert_eq!(case.run().chosen, Some(Uuid::from_u128(5)));
}

#[test]
fn a_limit_the_driver_cannot_hold_rejects_the_node() {
    let mut case = Case::new(vec![node(1, &[])]);
    case.body.resources.pids_limit = 512;
    let e = case.run();
    assert_eq!(checks(&e, 1), [Check::Enforcement]);
    assert!(e.filtered[0].detail.contains("pids"), "{}", e.filtered[0].detail);
}

#[test]
fn the_strategy_decides_between_a_full_node_and_an_empty_one() {
    let mut case = Case::new(vec![node(1, &[]), node(2, &[])]);
    case.allocated.insert(Uuid::from_u128(1), Bounds::from([(Dimension::Memory, 4 * GIB)]));
    case.policy.strategy = Strategy::Packed;
    assert_eq!(case.run().chosen, Some(Uuid::from_u128(1)));
    case.policy.strategy = Strategy::Distributed;
    assert_eq!(case.run().chosen, Some(Uuid::from_u128(2)));
    case.policy.strategy = Strategy::Balanced;
    let e = case.run();
    assert_eq!((e.chosen, e.runner_up), (Some(Uuid::from_u128(2)), Some(Uuid::from_u128(1))));
    assert!(e.scored.iter().any(|s| s.signal == Signal::Utilization));
    assert!(!e.scored.iter().any(|s| s.signal == Signal::ContentLocality), "not guessed");
}

#[test]
fn required_anti_affinity_keeps_replicas_apart_by_rack() {
    let mut case = Case::new(vec![
        node(1, &[("rack", "a")]),
        node(2, &[("rack", "a")]),
        node(3, &[("rack", "b")]),
        node(4, &[]),
    ]);
    let db =
        LabelSelector { match_labels: [("app".into(), "db".into())].into(), ..Default::default() };
    case.labels = [("app".to_owned(), "db".to_owned())].into();
    case.peers = vec![Peer {
        workload: Uuid::from_u128(500),
        project: PROJECT,
        node: Uuid::from_u128(1),
        labels: case.labels.clone(),
    }];
    case.body.placement.affinity = vec![AffinityRule {
        anti: true,
        required: true,
        workloads: db,
        topology_key: "rack".into(),
    }];
    let e = case.run();
    assert_eq!(checks(&e, 2), [Check::Affinity], "same rack as the peer");
    assert_eq!(checks(&e, 4), [Check::Affinity], "no rack label: nothing shows it holds");
    assert_eq!(e.chosen, Some(Uuid::from_u128(3)));
}

#[test]
fn spread_prefers_the_domain_that_keeps_skew_within_bounds() {
    let mut case = Case::new(vec![node(1, &[("rack", "a")]), node(2, &[("rack", "b")])]);
    case.labels = [("app".to_owned(), "web".to_owned())].into();
    let web = LabelSelector { match_labels: case.labels.clone(), ..Default::default() };
    case.peers = (0..2)
        .map(|i| Peer {
            workload: Uuid::from_u128(600 + i),
            project: PROJECT,
            node: Uuid::from_u128(1),
            labels: case.labels.clone(),
        })
        .collect();
    case.policy.strategy = Strategy::Packed;
    case.allocated.insert(Uuid::from_u128(1), Bounds::from([(Dimension::Memory, 2 * GIB)]));
    case.policy.spread = vec![SpreadConstraint {
        topology_key: "rack".into(),
        max_skew: 1,
        scope: SpreadScope::Selector(web),
    }];
    case.policy.weights.insert(Signal::FailureDomain, 10.0);
    assert_eq!(
        case.run().chosen,
        Some(Uuid::from_u128(2)),
        "packing would pick rack a; spread wins"
    );
}

#[test]
fn a_cordoned_node_keeps_what_is_already_there() {
    let mut here = node(1, &[]);
    here.admin = NodeAdmin::Cordoned;
    let reserved = Bounds::from([(Dimension::Memory, 6 * GIB), (Dimension::Cpu, 1_000)]);
    let allocated = reserved.clone();
    let labels = BTreeMap::new();
    let b = body(7 * GIB);
    let req = Request {
        workload: Uuid::from_u128(1000),
        project: PROJECT,
        labels: &labels,
        owners: &[ORG],
        body: &b,
        current: Some(Current { node: here.id, reserved: &reserved }),
        edge_tier: false,
        volumes: &[],
    };
    let e = place(
        &req,
        &[Candidate { node: &here, allocated: &allocated }],
        &[],
        &PlacementPolicy::default(),
    );
    assert_eq!(e.chosen, Some(here.id), "its own reservation is counted as free for itself");
    assert!(
        e.scored
            .iter()
            .any(|s| s.signal == Signal::VolumeLocality && (s.value - 1.0).abs() < f64::EPSILON)
    );
}

#[test]
fn reservation_counts_only_what_placement_guarantees() {
    let mut b = body(GIB);
    b.resources.pids_limit = 100;
    let r = reservation(&b);
    assert_eq!(r, Bounds::from([(Dimension::Cpu, 1_000), (Dimension::Memory, GIB)]));
}

fn req<'a>(labels: &'a BTreeMap<String, String>, b: &'a IntentBody) -> Request<'a> {
    Request {
        workload: Uuid::from_u128(1000),
        project: PROJECT,
        labels,
        owners: &[ORG],
        body: b,
        current: None,
        edge_tier: false,
        volumes: &[],
    }
}

#[test]
fn an_operator_overrides_their_own_decisions_about_a_node_and_nothing_else() {
    let mut chosen = node(1, &[]);
    chosen.admin = NodeAdmin::Cordoned;
    chosen.taints.push(Taint { key: "dedicated".into(), value: "ops".into() });
    let labels = BTreeMap::new();
    let mut b = body(GIB);
    let empty = Bounds::new();
    let c = Candidate { node: &chosen, allocated: &empty };
    let policy = PlacementPolicy::default();
    let scheduled = place(&req(&labels, &b), &[c], &[], &policy);
    assert_eq!(scheduled.chosen, None, "the scheduler respects the cordon and the taint");
    assert!(
        operator_refusals(&req(&labels, &b), &c, &[], &policy).is_empty(),
        "the operator overrides both"
    );

    b.resources.memory.reservation = 1 << 50;
    let refused = operator_refusals(&req(&labels, &b), &c, &[], &policy);
    assert_eq!(refused.iter().map(|r| r.check).collect::<Vec<_>>(), [Check::Capacity]);
    chosen.admin = NodeAdmin::Draining;
    let c = Candidate { node: &chosen, allocated: &empty };
    let refused = operator_refusals(&req(&labels, &body(GIB)), &c, &[], &policy);
    assert_eq!(refused.iter().map(|r| r.check).collect::<Vec<_>>(), [Check::Status], "not a drain");
}

#[test]
fn the_driver_layer_is_the_union_of_what_every_driver_that_could_run_the_tier_declares() {
    use crate::workload::plan::{DriverClass, StepAction};
    let declaring = |tier, actions: Vec<StepAction>| {
        let mut d = driver(&[]);
        d.capabilities.disruptive = vec![(tier, actions)];
        d
    };
    let mut vm_only = declaring(IsolationTier::Vm, vec![StepAction::Restart]);
    vm_only.capabilities.tiers = vec![IsolationTier::Vm];
    let drivers = [
        declaring(IsolationTier::Container, vec![StepAction::Stop]),
        declaring(IsolationTier::Container, vec![StepAction::Start, StepAction::Stop]),
        vm_only,
    ];
    let class = DriverClass::of(&drivers, IsolationTier::Container);
    assert_eq!(class.disruptive, [StepAction::Start, StepAction::Stop]);
    assert_eq!(DriverClass::of(&drivers, IsolationTier::Vm).disruptive, [StepAction::Restart]);
    assert!(DriverClass::of(&drivers, IsolationTier::Process).disruptive.is_empty());
}

#[test]
fn an_existing_volume_keeps_the_workload_on_the_node_holding_it() {
    let (one, two) = (node(1, &[]), node(2, &[]));
    let labels = BTreeMap::new();
    let b = body(GIB);
    let held = [(Uuid::from_u128(77), two.id)];
    let request = Request { volumes: &held, ..req(&labels, &b) };
    let empty = Bounds::new();
    let nodes =
        [Candidate { node: &one, allocated: &empty }, Candidate { node: &two, allocated: &empty }];
    let e = place(&request, &nodes, &[], &PlacementPolicy::default());
    assert_eq!(e.chosen, Some(two.id));
    assert!(e.filtered.iter().any(|r| r.node == one.id && r.check == Check::Volume));
}
