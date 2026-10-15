// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Prepare` and `Describe`: the spec a thin recipe becomes, the refusals that name why, and
//! the capabilities as the code keeps them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_driver_oci::containerd::signal_number;
use korpis_driver_oci::describe::{SIGNALS, info};
use korpis_driver_oci::prepare::prepare;
use korpis_driver_oci::validate::DEFAULT_PIDS;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::value::Kind as V;

const IMAGE: &str = "docker.io/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e";

fn recipe(extra_field: &str) -> Vec<u8> {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: hello, version: "1.0.0", description: hello, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64, riscv64, s390x, arm, "386"], image: "{IMAGE}"}}
  entry: {{command: [/bin/sh, -c, "exec httpd -f"], working_dir: /srv}}
config:
  fields:
    - {{key: greeting, type: string, default: hi, deliver: env, deliver_as: GREETING}}
    - {{key: port, type: integer, default: 8080, deliver: arg, deliver_as: "-p {{}}"}}
{extra_field}"#
    )
    .into_bytes()
}

fn intent(tier: pb::IsolationTier) -> pb::Intent {
    let text = |s: &str| pb::Value { kind: Some(V::StringValue(s.into())), ..Default::default() };
    let int = |i| pb::Value { kind: Some(V::IntValue(i)), ..Default::default() };
    let body = pb::IntentBody {
        runtime: pb::RuntimeSpec { driver: "oci".into(), tier: tier.into(), ..Default::default() }
            .into(),
        volumes: vec![pb::VolumeSpec {
            name: "data".into(),
            path: "/srv".into(),
            mode: pb::VolumeMode::VOLUME_MODE_RO.into(),
            ..Default::default()
        }],
        config: [("greeting".to_owned(), text("hello")), ("port".to_owned(), int(80))].into(),
        ..Default::default()
    };
    pb::Intent { id: "in_1".into(), body: body.into(), ..Default::default() }
}

const CONTAINER: pb::IsolationTier = pb::IsolationTier::ISOLATION_TIER_CONTAINER;

#[test]
fn a_thin_recipe_becomes_the_spec_create_would_build() {
    let spec = prepare("wl_1", &intent(CONTAINER), &recipe("")).unwrap();
    assert_eq!(spec.rootfs, IMAGE);
    assert_eq!(spec.command, ["/bin/sh", "-c", "exec httpd -f"]);
    assert_eq!(spec.args, ["-p", "80"], "the intent's value, not the default");
    assert_eq!(spec.env.get("GREETING").map(String::as_str), Some("hello"));
    assert_eq!(spec.working_dir, "/srv");
    assert_eq!(spec.resources.as_option().unwrap().pids.as_option().unwrap().limit, DEFAULT_PIDS);
    let m = &spec.mounts[0];
    assert_eq!((m.volume.as_str(), m.destination.as_str(), m.read_only), ("data", "/srv", true));
    assert!(m.source.is_empty(), "where a volume lives is the helper's to say");
    assert_eq!(spec, prepare("wl_1", &intent(CONTAINER), &recipe("")).unwrap(), "pure");
}

#[test]
fn what_this_driver_cannot_run_is_refused_with_the_reason() {
    type R = pb::UnsatisfiableReason;
    let reason = |i: &pb::Intent, m: &[u8]| prepare("wl_1", i, m).unwrap_err().reason.as_known();
    let vm = intent(pb::IsolationTier::ISOLATION_TIER_VM);
    assert_eq!(reason(&vm, &recipe("")), Some(R::UNSATISFIABLE_REASON_MISSING_DRIVER));
    let mut other = intent(CONTAINER);
    other.body.get_or_insert_default().runtime.get_or_insert_default().driver = "qemu".into();
    assert_eq!(reason(&other, &recipe("")), Some(R::UNSATISFIABLE_REASON_MISSING_DRIVER));
    let garbage = reason(&intent(CONTAINER), b"not a recipe");
    assert_eq!(garbage, Some(R::UNSATISFIABLE_REASON_PREPARE_REJECTED));
    let managed = "    - {key: motd, type: string, default: x, deliver: managed_file}\n\
                   managed_files:\n  - {name: motd.txt, from: motd.txt, format: ini, vars: [motd]}\n";
    let mut with_motd = intent(CONTAINER);
    let motd = pb::Value { kind: Some(V::StringValue("x".into())), ..Default::default() };
    with_motd.body.get_or_insert_default().config.insert("motd".into(), motd);
    let e = prepare("wl_1", &with_motd, &recipe(managed)).unwrap_err();
    assert!(e.detail.contains("config.motd"), "{}", e.detail);
}

#[test]
fn the_declaration_matches_what_the_code_keeps() {
    let info = info(true);
    let caps = info.capabilities.as_option().unwrap();
    for s in SIGNALS {
        assert!(signal_number(s).is_ok(), "{s} is declared and accepted");
    }
    assert!(signal_number("SIGSTOP").is_err(), "and nothing undeclared is");
    let enforced = |k: &str| caps.enforcement.get(k).map(|d| d.enforced);
    assert_eq!(enforced("memory"), Some(true));
    assert_eq!(enforced("pids"), Some(true));
    assert_eq!(enforced("disk"), Some(false), "displayed nowhere, so promised nowhere");
    assert!(caps.id_mapping.as_option().unwrap().idmapped_mounts);
    assert_eq!(caps.tiers.len(), 1);
}
