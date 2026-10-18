// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Minecraft server of §3 of docs/13-surface-cli.md, as an intent body.

use korpis_core::recipe::{Disruption, DisruptionDecl, RecipeRef};
use korpis_core::value::{Value, ValueMap};
use korpis_core::workload::plan::{Classifier, DriverClass};
use korpis_core::workload::spec::endpoints::{Exposure, NetworkPolicy, Protocol, ShapingSpec};
use korpis_core::workload::spec::resources::{CpuSpec, ReservedLimit};
use korpis_core::workload::spec::volumes::VolumeMode;
use korpis_core::workload::spec::{
    ConsoleMode, Continuity, EndpointSpec, HealthSpec, IntentBody, IsolationTier, Lifecycle,
    PlacementSpec, ResourceSpec, RuntimeSpec, VolumeSpec,
};

pub const GIB: u64 = 1 << 30;

pub fn body() -> IntentBody {
    let resources = ResourceSpec {
        memory: ReservedLimit { reservation: 4 * GIB, limit: 6 * GIB },
        cpu: CpuSpec { reservation_millis: 2000, ..CpuSpec::default() },
        ..ResourceSpec::default()
    };
    IntentBody {
        recipe: RecipeRef {
            name: "ghcr.io/korpis/recipes/minecraft-paper".into(),
            version: "1.21.4".into(),
            digest: format!("sha256:{}", "9d2f".repeat(16)),
            trust: Some(korpis_core::recipe::trust::Vouched::Signer("korpis".into())),
            providers: Vec::new(),
        },
        lifecycle: Lifecycle::Persistent,
        schedule: None,
        timezone: None,
        runs: None,
        console: ConsoleMode::Interactive,
        runtime: RuntimeSpec {
            driver: "oci".into(),
            tier: IsolationTier::Container,
            config: ValueMap::new(),
        },
        resources,
        volumes: vec![VolumeSpec {
            name: "world".into(),
            path: "/data".into(),
            size_bytes: 40 * GIB,
            storage_class: "replicated".into(),
            mode: VolumeMode::ReadWrite,
            existing: None,
            from_backup: None,
        }],
        endpoints: vec![EndpointSpec {
            name: "game".into(),
            protocol: Protocol::Tcp,
            container_port: 25565,
            exposure: Exposure::Stable,
            domains: Vec::new(),
            tls: None,
            policy: NetworkPolicy::default(),
            accounting: false,
            shaping: ShapingSpec::default(),
        }],
        health: HealthSpec::default(),
        config: ValueMap::from([("motd".to_owned(), Value::String("Survival".into()))]),
        placement: PlacementSpec::default(),
        continuity: Continuity::Derived,
        dependencies: Vec::new(),
        streams: korpis_core::workload::spec::StreamSpec::default(),
        egress: korpis_core::workload::spec::EgressPolicy::default(),
    }
}

/// A game server: stopping and restarting drop players, and so does changing `difficulty`.
pub fn game_server() -> Disruption {
    let players = |r: &str| Some(DisruptionDecl { disruptive: true, reason: r.into() });
    Disruption {
        stop: players("connected players are dropped"),
        restart: players("connected players are dropped"),
        disruptive_fields: vec!["difficulty".into()],
    }
}

pub fn classifier<'a>(driver: &'a DriverClass, recipe: &'a Disruption) -> Classifier<'a> {
    Classifier { driver, recipe, overrides: &[] }
}
