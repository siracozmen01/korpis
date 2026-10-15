// SPDX-License-Identifier: AGPL-3.0-or-later

//! A declaration completed by its recipe (§3.1 of docs/09-recipes.md).

use korpis_core::recipe::behaviour::LifecycleHooks;
use korpis_core::recipe::runtime::{
    Entry, Preset, PresetEndpoint, PresetResources, PresetVolume, RecipeRuntime, RuntimeTarget,
    TargetSource,
};
use korpis_core::recipe::{ConfigSchema, Disruption, Metadata, Recipe, RecipeRef};
use korpis_core::workload::declare::{
    Declaration, DeclaredEndpoint, DeclaredRuntime, DeclaredVolume,
};
use korpis_core::workload::spec::endpoints::{Exposure, Protocol};
use korpis_core::workload::spec::health::{HealthSpec, Probe, ProbeKind};
use korpis_core::workload::spec::resources::ReservedLimit;
use korpis_core::workload::spec::{ConsoleMode, IsolationTier, Lifecycle};

const GIB: u64 = 1 << 30;

fn paper() -> Recipe {
    let endpoint = |name: &str, protocol, port, exposure_default| PresetEndpoint {
        name: name.into(),
        protocol,
        port,
        exposure_default,
    };
    Recipe {
        metadata: Metadata {
            name: "paper".into(),
            version: "1.21.4".into(),
            ..Metadata::default()
        },
        runtime: RecipeRuntime {
            targets: vec![RuntimeTarget {
                tier: IsolationTier::Container,
                arch: vec!["amd64".into()],
                source: TargetSource::Image(format!(
                    "docker.io/library/temurin@sha256:{}",
                    "a".repeat(64)
                )),
            }],
            entry: Entry::default(),
        },
        preset: Preset {
            lifecycle: Lifecycle::Persistent,
            console: ConsoleMode::Interactive,
            endpoints: vec![
                endpoint("game", Protocol::Tcp, 25565, Exposure::Stable),
                endpoint("query", Protocol::Udp, 25565, Exposure::Stable),
            ],
            volumes: vec![PresetVolume {
                name: "data".into(),
                path: "/data".into(),
                size_default_bytes: 10 * GIB,
            }],
            resources: PresetResources {
                memory_min_bytes: GIB,
                memory_recommended_bytes: 4 * GIB,
                cpu_min_millis: 500,
                cpu_recommended_millis: 2000,
            },
        },
        config: ConfigSchema::default(),
        install: Vec::new(),
        templates: Vec::new(),
        managed_files: Vec::new(),
        probes: HealthSpec {
            readiness: Some(Probe {
                kind: ProbeKind::Tcp { endpoint: "game".into() },
                period: None,
                timeout: None,
                failures: 0,
                initial_delay: None,
            }),
            liveness: None,
        },
        logs: Vec::new(),
        lifecycle_hooks: LifecycleHooks::default(),
        disruption: Disruption::default(),
    }
}

fn reference() -> RecipeRef {
    RecipeRef {
        name: "korpis.io/paper".into(),
        version: "1.21.4".into(),
        digest: format!("sha256:{}", "b".repeat(64)),
        trust: None,
        providers: Vec::new(),
    }
}

#[test]
fn an_empty_declaration_is_the_recipe_preset() {
    let body = Declaration::default().complete(reference(), &paper()).unwrap();
    assert_eq!((body.lifecycle, body.console), (Lifecycle::Persistent, ConsoleMode::Interactive));
    assert_eq!(
        (body.runtime.driver.as_str(), body.runtime.tier),
        ("oci", IsolationTier::Container)
    );
    assert_eq!(body.resources.memory.reservation, 4 * GIB, "the recommendation is reserved");
    assert_eq!(body.resources.cpu.reservation_millis, 2000);
    assert_eq!(body.volumes.len(), 1);
    assert_eq!((body.volumes[0].path.as_str(), body.volumes[0].size_bytes), ("/data", 10 * GIB));
    let names: Vec<_> = body.endpoints.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["game", "query"]);
    assert_eq!(body.health, paper().probes, "the recipe's probes");
}

#[test]
fn what_is_declared_wins_and_what_is_left_out_comes_from_the_preset() {
    let d = Declaration {
        console: Some(ConsoleMode::Logs),
        volumes: vec![DeclaredVolume {
            name: "data".into(),
            size_bytes: Some(40 * GIB),
            ..Default::default()
        }],
        endpoints: vec![
            DeclaredEndpoint {
                name: "game".into(),
                exposure: Some(Exposure::NodePort),
                ..Default::default()
            },
            DeclaredEndpoint {
                name: "map".into(),
                protocol: Some(Protocol::Http),
                container_port: Some(8123),
                ..Default::default()
            },
        ],
        ..Declaration::default()
    };
    let body = d.complete(reference(), &paper()).unwrap();
    assert_eq!(body.console, ConsoleMode::Logs);
    assert_eq!((body.volumes[0].path.as_str(), body.volumes[0].size_bytes), ("/data", 40 * GIB));
    let game = &body.endpoints[0];
    assert_eq!((game.container_port, game.exposure), (25565, Exposure::NodePort));
    let map = body.endpoints.iter().find(|e| e.name == "map").unwrap();
    assert_eq!(map.exposure, Exposure::Stable, "the default exposure (§3 of 07-networking.md)");
}

#[test]
fn a_declaration_the_recipe_cannot_run_is_refused() {
    let below = Declaration {
        resources: korpis_core::workload::spec::ResourceSpec {
            memory: ReservedLimit { reservation: GIB / 2, limit: 0 },
            ..Default::default()
        },
        ..Declaration::default()
    };
    assert_eq!(below.complete(reference(), &paper()).unwrap_err().field, "resources.memory");
    let vm = Declaration {
        runtime: DeclaredRuntime { tier: Some(IsolationTier::Vm), ..Default::default() },
        ..Declaration::default()
    };
    assert_eq!(vm.complete(reference(), &paper()).unwrap_err().field, "runtime.tier");
    let unnamed = Declaration {
        endpoints: vec![DeclaredEndpoint { name: "extra".into(), ..Default::default() }],
        ..Declaration::default()
    };
    assert_eq!(unnamed.complete(reference(), &paper()).unwrap_err().field, "endpoints[0].protocol");
}
