// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::validate::{Policy, validate};
use korpis_core::workload::assignment::FIRST_WORKLOAD_ID;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use serde_json::Value;

pub(super) fn spec(command: &[&str]) -> Valid {
    joined(command, "")
}

/// As [`spec`], joining the namespaces in `namespaces`.
fn joined(command: &[&str], namespaces: &str) -> Valid {
    let s = driver::PreparedSpec {
        namespaces: namespaces.into(),
        workload: "wl_1".into(),
        intent: "in_1".into(),
        rootfs: format!("r.example/mc@sha256:{}", "a".repeat(64)),
        command: command.iter().map(|c| (*c).to_owned()).collect(),
        env: [("EULA".to_owned(), "true".to_owned())].into(),
        tier: pb::IsolationTier::ISOLATION_TIER_CONTAINER.into(),
        labels: driver::RuntimeLabels {
            workload: "wl_1".into(),
            intent: "in_1".into(),
            ..Default::default()
        }
        .into(),
        uid_base: FIRST_WORKLOAD_ID,
        uid_count: 1 << 16,
        resources: pb::ResourceSpec {
            memory: pb::ReservedLimit {
                reservation: 1 << 30,
                limit: 2 << 30,
                ..Default::default()
            }
            .into(),
            cpu: pb::CpuSpec { reservation_millis: 500, limit_millis: 2000, ..Default::default() }
                .into(),
            pids: pb::PidsSpec { limit: 512, ..Default::default() }.into(),
            ..Default::default()
        }
        .into(),
        mounts: vec![driver::Mount {
            source: "/v/wl_1.world".into(),
            destination: "/data".into(),
            volume: "world".into(),
            ..Default::default()
        }],
        identity_dir: "/i/wl_1".into(),
        ..Default::default()
    };
    let netns_root = std::path::Path::new(namespaces)
        .parent()
        .map_or_else(|| "/n".into(), std::path::Path::to_owned);
    let devices = crate::devices::Specs { dirs: Vec::new(), owner: 0 };
    let policy = Policy {
        volume_root: "/v".into(),
        shared_roots: Vec::new(),
        identity_root: "/i".into(),
        netns_root,
        devices,
    };
    validate(s, &policy).unwrap()
}

pub(super) fn image() -> ImageConfig {
    ImageConfig {
        env: vec!["PATH=/bin".into(), "EULA=false".into()],
        entrypoint: vec!["/start".into()],
        cmd: vec!["--nogui".into()],
        user: (1000, 1000),
        ..ImageConfig::default()
    }
}

#[test]
fn the_tiers_hardening_is_always_there() {
    let j: Value = serde_json::to_value(
        runtime_spec(&spec(&[]), &image(), "/korpis/x", &Given::default()).unwrap(),
    )
    .unwrap();
    assert_eq!(j["root"]["readonly"], true);
    assert_eq!(j["process"]["noNewPrivileges"], true);
    assert_eq!(j["linux"]["uidMappings"][0]["hostId"], u64::from(FIRST_WORKLOAD_ID));
    assert_eq!(j["linux"]["uidMappings"][0]["size"], 65536);
    let caps = j["process"]["capabilities"]["bounding"].as_array().unwrap();
    assert!(!caps.iter().any(|c| c == "CAP_SYS_ADMIN" || c == "CAP_NET_RAW"));
    let ns: Vec<&str> = j["linux"]["namespaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["type"].as_str().unwrap())
        .collect();
    assert!(ns.contains(&"user") && ns.contains(&"network"));
    assert_eq!(j["linux"]["seccomp"]["defaultAction"], "SCMP_ACT_ERRNO");
    assert!(j["linux"]["maskedPaths"].as_array().unwrap().iter().any(|p| p == "/proc/kcore"));
    let volume =
        j["mounts"].as_array().unwrap().iter().find(|m| m["destination"] == "/data").unwrap();
    assert_eq!(
        volume["uidMappings"][0]["hostId"],
        u64::from(FIRST_WORKLOAD_ID),
        "an idmapped mount"
    );
    let identity =
        j["mounts"].as_array().unwrap().iter().find(|m| m["destination"] == "/run/korpis").unwrap();
    assert_eq!(identity["source"], "/i/wl_1");
    assert!(identity["options"].as_array().unwrap().iter().any(|o| o == "ro"));
    assert!(identity.get("uidMappings").is_none_or(|m| m.as_array().is_none_or(Vec::is_empty)));
    let r = &j["linux"]["resources"];
    assert_eq!(
        (r["memory"]["limit"].as_i64(), r["pids"]["limit"].as_i64()),
        (Some(2 << 30), Some(512))
    );
    assert_eq!(
        (r["cpu"]["quota"].as_i64(), r["cpu"]["shares"].as_u64()),
        (Some(200_000), Some(512))
    );
}

#[test]
fn the_command_and_environment_merge_the_recipe_over_the_image() {
    let built = runtime_spec(&spec(&[]), &image(), "/k", &Given::default()).unwrap();
    assert_eq!(built.process.args, vec!["/start", "--nogui"]);
    assert_eq!(built.process.env, vec!["PATH=/bin", "EULA=true"], "the recipe's variable wins");
    assert_eq!((built.process.user.uid, built.process.cwd.as_str()), (1000, "/"));
    let own = runtime_spec(&spec(&["/bin/sh", "-c"]), &image(), "/k", &Given::default()).unwrap();
    assert_eq!(own.process.args, vec!["/bin/sh", "-c"]);
    let nothing = ImageConfig::default();
    assert!(runtime_spec(&spec(&[]), &nothing, "/k", &Given::default()).is_err(), "nothing to run");
}

#[test]
fn a_workload_given_a_network_joins_its_namespaces_and_still_states_its_mapping() {
    let j: Value = serde_json::to_value(
        runtime_spec(&joined(&[], "/n/wl_1"), &image(), "/korpis/x", &Given::default()).unwrap(),
    )
    .unwrap();
    let namespaces = j["linux"]["namespaces"].as_array().unwrap();
    let path = |kind: &str| namespaces.iter().find(|n| n["type"] == kind).unwrap()["path"].clone();
    assert_eq!(path("network"), "/n/wl_1/net");
    assert_eq!(path("user"), "/n/wl_1/user");
    assert!(path("pid").is_null(), "every other namespace is its own");
    assert_eq!(j["linux"]["uidMappings"][0]["hostId"], u64::from(FIRST_WORKLOAD_ID));
    let own: Value = serde_json::to_value(
        runtime_spec(&spec(&[]), &image(), "/korpis/x", &Given::default()).unwrap(),
    )
    .unwrap();
    let made = own["linux"]["namespaces"].as_array().unwrap();
    assert!(made.iter().all(|n| n.get("path").is_none()), "nothing joined without a network");
}

#[test]
fn a_network_s_resolv_conf_is_mounted_read_only_when_it_has_one() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("wl_1");
    std::fs::create_dir(&dir).unwrap();
    let net = dir.to_string_lossy().into_owned();
    let mounts = |net: &str| -> Vec<Value> {
        let spec =
            runtime_spec(&joined(&[], net), &image(), "/korpis/x", &Given::default()).unwrap();
        let j = serde_json::to_value(spec).unwrap();
        j["mounts"].as_array().unwrap().clone()
    };
    let resolv = |ms: &[Value]| ms.iter().find(|m| m["destination"] == "/etc/resolv.conf").cloned();
    assert!(resolv(&mounts(&net)).is_none(), "a network made without one gives none");
    std::fs::write(dir.join("resolv.conf"), "nameserver 169.254.1.53\n").unwrap();
    let m = resolv(&mounts(&net)).expect("mounted");
    assert_eq!(m["source"], format!("{net}/resolv.conf"));
    assert!(m["options"].as_array().unwrap().iter().any(|o| o == "ro"));
}

#[test]
fn a_devices_node_is_bound_and_allowed_and_its_edits_reach_the_spec() {
    let given = Given {
        nodes: vec![crate::devices::Bound {
            host: "/dev/null".into(),
            path: "/dev/standin".into(),
            numbers: (1, 3),
            access: "rw".into(),
        }],
        env: vec!["STANDIN=1".into()],
        gids: vec![44],
        hooks: vec![korpis_cdi::spec::Hook {
            hook_name: "createContainer".into(),
            path: "/usr/bin/vendor-hook".into(),
            args: vec!["vendor-hook".into()],
            env: Vec::new(),
            timeout: None,
        }],
        ..Given::default()
    };
    let built = runtime_spec(&spec(&[]), &image(), "/k", &given).unwrap();
    let json = serde_json::to_value(&built).unwrap();
    let bound =
        json["mounts"].as_array().unwrap().iter().find(|m| m["destination"] == "/dev/standin");
    assert_eq!(bound.unwrap()["source"], "/dev/null");
    let rules = json["linux"]["resources"]["devices"].as_array().unwrap();
    assert_eq!(rules[0]["allow"], false, "every device denied first");
    assert_eq!(
        (&rules[1]["major"], &rules[1]["minor"], &rules[1]["access"]),
        (&Value::from(1), &Value::from(3), &Value::from("rw"))
    );
    assert!(json["process"]["env"].as_array().unwrap().contains(&Value::from("STANDIN=1")));
    assert_eq!(json["process"]["user"]["additionalGids"], serde_json::json!([44]));
    assert_eq!(json["hooks"]["createContainer"][0]["path"], "/usr/bin/vendor-hook");
    let plain =
        serde_json::to_value(runtime_spec(&spec(&[]), &image(), "/k", &Given::default()).unwrap())
            .unwrap();
    assert!(plain.get("hooks").is_none(), "no hooks without a device's");
}
