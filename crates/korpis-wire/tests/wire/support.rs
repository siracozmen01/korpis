// SPDX-License-Identifier: AGPL-3.0-or-later

//! A body that sets at least one of every kind of thing the schema can carry.

use korpis_core::labels::{LabelExpression, LabelSelector};
use korpis_core::recipe::RecipeRef;
use korpis_core::value::{Value, ValueMap};
use korpis_core::workload::spec::Dependency;
use korpis_core::workload::spec::dependencies::{DependencyTarget, Injection};
use korpis_core::workload::spec::endpoints::{
    ConntrackLimits, Exposure, NetworkAction, NetworkPolicy, NetworkRule, Peer, PortRange,
    Protocol, ShapingSpec, TlsSpec,
};
use korpis_core::workload::spec::health::{Probe, ProbeKind};
use korpis_core::workload::spec::placement::{AffinityRule, Toleration};
use korpis_core::workload::spec::resources::{CpuIsolation, CpuSpec, ReservedLimit};
use korpis_core::workload::spec::volumes::VolumeMode;
use korpis_core::workload::spec::{
    ConsoleMode, Continuity, EndpointSpec, HealthSpec, IntentBody, IsolationTier, Lifecycle,
    PlacementSpec, ResourceSpec, RuntimeSpec, VolumeSpec,
};
use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

const GIB: u64 = 1 << 30;

fn endpoint(name: &str, protocol: Protocol, port: u16) -> EndpointSpec {
    EndpointSpec {
        name: name.into(),
        protocol,
        container_port: port,
        exposure: Exposure::Stable,
        domains: Vec::new(),
        tls: None,
        policy: NetworkPolicy::default(),
        accounting: false,
        shaping: ShapingSpec::default(),
    }
}

/// TLS the proxy passes through, its workload told who connected by a PROXY header.
pub fn tunnel() -> EndpointSpec {
    EndpointSpec {
        exposure: Exposure::Ingress,
        domains: vec!["tunnel.example.com".into()],
        tls: Some(TlsSpec::Passthrough { proxy_protocol: true }),
        ..endpoint("tunnel", Protocol::Tcp, 8443)
    }
}

/// Where the fixture's workload may connect out.
pub fn egress() -> korpis_core::workload::spec::EgressPolicy {
    let rule = NetworkRule {
        action: NetworkAction::Allow,
        peer: Peer::Workloads(LabelSelector {
            match_labels: BTreeMap::from([("tier".into(), "db".into())]),
            expressions: vec![LabelExpression::In("zone".into(), vec!["a".into()])],
        }),
        ports: vec![PortRange { first: 5432, last: 5432 }],
        protocol: Some(Protocol::Tcp),
    };
    let limits = ConntrackLimits { new_per_sec: 50, ..ConntrackLimits::default() };
    korpis_core::workload::spec::EgressPolicy { rules: vec![rule], limits }
}

fn web() -> EndpointSpec {
    EndpointSpec {
        domains: vec!["map.example.com".into()],
        tls: Some(TlsSpec::Acme { clients: None }),
        policy: NetworkPolicy {
            ingress: vec![NetworkRule {
                action: NetworkAction::Deny,
                peer: Peer::Cidr("10.0.0.0/8".parse().unwrap()),
                ports: Vec::new(),
                protocol: None,
            }],
            egress: Vec::new(),
            conntrack: ConntrackLimits { max_connections: 1000, ..ConntrackLimits::default() },
        },
        accounting: true,
        ..endpoint("map", Protocol::Https, 8443)
    }
}

fn health() -> HealthSpec {
    HealthSpec {
        readiness: Some(Probe {
            kind: ProbeKind::Http { endpoint: "map".into(), path: "/".into(), expect_status: 200 },
            period: Some(Duration::from_secs(10)),
            timeout: None,
            failures: 3,
            initial_delay: Some(Duration::from_millis(1500)),
        }),
        liveness: Some(Probe {
            kind: ProbeKind::Heartbeat { max_interval: Duration::from_secs(60) },
            period: None,
            timeout: None,
            failures: 0,
            initial_delay: None,
        }),
    }
}

fn config() -> ValueMap {
    ValueMap::from([
        ("motd".to_owned(), Value::String("Survival".into())),
        ("max_players".to_owned(), Value::Int(20)),
        ("view".to_owned(), Value::Double(0.5)),
        ("hardcore".to_owned(), Value::Bool(false)),
        ("seed".to_owned(), Value::Bytes(vec![1, 2, 3])),
        ("ops".to_owned(), Value::List(vec![Value::String("ana".into()), Value::Null])),
        ("rates".to_owned(), Value::Map(BTreeMap::from([("spawn".to_owned(), Value::Int(0))]))),
    ])
}

fn placement() -> PlacementSpec {
    PlacementSpec {
        node_selector: LabelSelector {
            match_labels: BTreeMap::from([("region".into(), "eu".into())]),
            expressions: Vec::new(),
        },
        tolerations: vec![Toleration { key: "gpu".into(), value: String::new() }],
        affinity: vec![AffinityRule {
            anti: true,
            required: false,
            workloads: LabelSelector::default(),
            topology_key: "host".into(),
        }],
        preferred_region: "eu-central".into(),
    }
}

/// One dependency of each kind of target.
fn dependencies() -> Vec<Dependency> {
    let selector = LabelSelector {
        match_labels: [("tier".to_owned(), "db".to_owned())].into(),
        ..Default::default()
    };
    vec![
        Dependency {
            name: "db".into(),
            target: DependencyTarget::Workload(Uuid::from_u128(7)),
            inject: Injection::Env,
            required: true,
        },
        Dependency {
            name: "cache".into(),
            target: DependencyTarget::Endpoint(Uuid::from_u128(8)),
            inject: Injection::File,
            required: false,
        },
        Dependency {
            name: "peers".into(),
            target: DependencyTarget::Selector(selector),
            inject: Injection::Both,
            required: false,
        },
    ]
}

/// A resolved recipe reference with one step provider pinned.
fn recipe() -> RecipeRef {
    RecipeRef {
        name: "ghcr.io/korpis/recipes/minecraft-paper".into(),
        version: "1.21.4".into(),
        digest: format!("sha256:{}", "9d2f".repeat(16)),
        trust: Some(korpis_core::recipe::trust::Vouched::Signer("korpis".into())),
        providers: vec![korpis_core::recipe::ProviderPin {
            step: "valve/steam.app".into(),
            extension: uuid::Uuid::from_u128(7),
            digest: format!("sha256:{}", "5d0e".repeat(16)),
        }],
    }
}

pub fn body() -> IntentBody {
    IntentBody {
        recipe: recipe(),
        lifecycle: Lifecycle::Persistent,
        schedule: None,
        timezone: None,
        runs: None,
        console: ConsoleMode::Interactive,
        runtime: RuntimeSpec {
            driver: "oci".into(),
            tier: IsolationTier::Container,
            config: ValueMap::from([("seccomp".into(), Value::String("default".into()))]),
        },
        resources: ResourceSpec {
            memory: ReservedLimit { reservation: 4 * GIB, limit: 6 * GIB },
            cpu: CpuSpec {
                reservation_millis: 2000,
                limit_millis: 0,
                isolation: CpuIsolation::DedicatedCores,
            },
            pids_limit: 4096,
            ..ResourceSpec::default()
        },
        volumes: vec![
            VolumeSpec {
                name: "world".into(),
                path: "/data".into(),
                size_bytes: 40 * GIB,
                storage_class: "replicated".into(),
                mode: VolumeMode::ReadWrite,
                existing: None,
                from_backup: Some(Uuid::now_v7()),
            },
            VolumeSpec {
                name: "maps".into(),
                path: "/maps".into(),
                size_bytes: GIB,
                storage_class: "local".into(),
                mode: VolumeMode::ReadOnly,
                existing: Some(Uuid::now_v7()),
                from_backup: None,
            },
        ],
        endpoints: vec![endpoint("game", Protocol::Tcp, 25565), web(), tunnel()],
        health: health(),
        config: config(),
        placement: placement(),
        continuity: Continuity::KeepRunning,
        dependencies: dependencies(),
        streams: korpis_core::workload::spec::StreamSpec {
            rate: 1024 * 1024,
            burst: 8 * 1024 * 1024,
            retention: korpis_core::workload::spec::streams::Retention::Duration(
                std::time::Duration::from_secs(86_400),
            ),
            offload_retention: std::time::Duration::from_secs(7 * 86_400),
        },
        egress: egress(),
    }
}
