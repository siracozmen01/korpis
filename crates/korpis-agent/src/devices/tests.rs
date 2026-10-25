// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

/// A file of NVIDIA's shape: two GPUs, a MIG partition of the first, the aggregate `all`, and
/// the control node and a hook given with every device.
fn nvidia(hook: &Path) -> String {
    format!(
        r#"cdiVersion: 0.5.0
kind: nvidia.com/gpu
devices:
  - name: "0"
    containerEdits:
      deviceNodes:
        - path: /dev/nvidia0
  - name: "1"
    containerEdits:
      deviceNodes:
        - path: /dev/nvidia1
  - name: "0:0"
    annotations:
      profile: 1g.10gb
    containerEdits:
      deviceNodes:
        - path: /dev/nvidia0
        - path: /dev/nvidia-caps/nvidia-cap21
  - name: all
    containerEdits:
      deviceNodes:
        - path: /dev/nvidia0
        - path: /dev/nvidia1
containerEdits:
  env:
    - NVIDIA_VISIBLE_DEVICES=void
  deviceNodes:
    - path: /dev/nvidiactl
  hooks:
    - hookName: createContainer
      path: {}
      args: [nvidia-cdi-hook, update-ldcache]
"#,
        hook.display()
    )
}

/// The directories `dirs`, owned by whoever runs the test, as a node's are by root.
fn config(dirs: &[&Path]) -> Config {
    use std::os::unix::fs::MetadataExt as _;
    let owner = std::fs::metadata(dirs[0]).unwrap().uid();
    Config { dirs: dirs.iter().map(|d| d.to_path_buf()).collect(), owner, ..Config::default() }
}

/// A program for hooks, writable by its owner alone.
fn program(dir: &Path) -> PathBuf {
    let p = dir.join("hook");
    std::fs::write(&p, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn reason<'a>(c: &'a Catalog, id: &str) -> &'a str {
    c.unoffered.iter().find(|u| u.id.ends_with(id)).map_or("", |u| u.reason.as_str())
}

#[test]
fn a_vendors_file_gives_its_gpus_and_partitions_and_not_its_aggregate() {
    let (specs, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(specs.path().join("nvidia.yaml"), nvidia(&program(bin.path()))).unwrap();
    let c = read(&config(&[specs.path()]));
    let ids: Vec<&str> = c.devices.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["nvidia.com/gpu=0", "nvidia.com/gpu=0:0", "nvidia.com/gpu=1"]);
    assert!(c.devices.iter().all(|d| d.class == pb::DeviceClass::DEVICE_CLASS_GPU));
    assert_eq!(c.devices[0].host_nodes, ["/dev/nvidia0"], "the control node is not its own");
    assert_eq!(c.devices[1].attributes.get("profile").map(String::as_str), Some("1g.10gb"));
    assert!(reason(&c, "gpu=all").contains("aggregate of 2"), "{c:?}");
    let edits = &c.edits["nvidia.com/gpu=0"];
    let nodes: Vec<&str> = edits.device_nodes.iter().map(|n| n.path.as_str()).collect();
    assert_eq!(nodes, ["/dev/nvidiactl", "/dev/nvidia0"], "the file's edits, then its own");
    assert_eq!((edits.env.len(), edits.hooks.len()), (1, 1));
}

#[test]
fn a_file_or_hook_others_may_write_is_not_read_and_nor_is_a_field_cdi_lacks() {
    let (specs, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let hook = program(bin.path());
    let at = specs.path().join("nvidia.yaml");
    std::fs::write(&at, nvidia(&hook)).unwrap();
    std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o666)).unwrap();
    let c = read(&config(&[specs.path()]));
    assert!(c.devices.is_empty() && reason(&c, "nvidia.yaml").contains("may write"), "{c:?}");
    std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o777)).unwrap();
    let c = read(&config(&[specs.path()]));
    assert!(c.devices.is_empty() && reason(&c, "gpu=0").contains("a hook is not run"), "{c:?}");
    let typo = r#"{"cdiVersion":"1.1.0","kind":"a.com/gpu","devices":[{"name":"0",
        "containerEdits":{"deviceNodes":[{"path":"/dev/a0"}],"devicenodes":[]}}]}"#;
    std::fs::write(&at, "").unwrap();
    std::fs::write(specs.path().join("a.json"), typo).unwrap();
    let c = read(&config(&[specs.path()]));
    assert!(reason(&c, "a.json").contains("devicenodes"), "{c:?}");
}

/// A file of kind `kind` with one device, `name`, giving `node`.
fn one(kind: &str, name: &str, node: &str) -> String {
    format!(
        r#"{{"cdiVersion":"0.6.0","kind":"{kind}","annotations":{{"vendor":"x"}},
            "devices":[{{"name":"{name}",
                "containerEdits":{{"deviceNodes":[{{"path":"{node}"}}]}}}}]}}"#
    )
}

#[test]
fn a_later_directory_replaces_a_device_and_one_directory_describing_it_twice_offers_neither() {
    let (low, high) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(low.path().join("a.json"), one("a.com/gpu", "0", "/dev/low")).unwrap();
    std::fs::write(high.path().join("a.json"), one("a.com/gpu", "0", "/dev/high")).unwrap();
    let c = read(&config(&[low.path(), high.path()]));
    assert_eq!(c.devices[0].host_nodes, ["/dev/high"]);
    std::fs::write(high.path().join("b.json"), one("a.com/gpu", "0", "/dev/other")).unwrap();
    let c = read(&config(&[low.path(), high.path()]));
    assert!(c.devices.is_empty() && reason(&c, "gpu=0").contains("twice"), "{c:?}");
}

#[test]
fn a_kind_naming_no_class_is_offered_once_the_operator_maps_it_and_shares_are_its_kinds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("s.json"), one("korpis.test/standin", "a", "/dev/zero"))
        .unwrap();
    std::fs::write(
        dir.path().join("n.json"),
        r#"{"cdiVersion":"1.0.0","kind":"b.com/gpu",
        "devices":[{"name":"0","containerEdits":{"env":["A=1"]}}]}"#,
    )
    .unwrap();
    let mut config = config(&[dir.path()]);
    let c = read(&config);
    assert!(reason(&c, "standin=a").contains("unclassified"), "{c:?}");
    assert!(reason(&c, "b.com/gpu=0").contains("no device node"), "{c:?}");
    config.classes.insert("korpis.test/standin".into(), DeviceClass::Serial);
    config.shares.insert("korpis.test/standin".into(), 3);
    let c = read(&config);
    assert_eq!(c.devices.len(), 1);
    assert_eq!(c.devices[0].class, pb::DeviceClass::DEVICE_CLASS_SERIAL);
    assert_eq!(c.devices[0].shares, 3);
    assert_eq!(c.devices[0].attributes.get("vendor").map(String::as_str), Some("x"));
}

#[test]
fn a_newer_version_or_a_missing_directory_is_said_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let newer = one("a.com/gpu", "0", "/dev/a").replace("0.6.0", "1.2.0");
    std::fs::write(dir.path().join("a.json"), newer).unwrap();
    let c = read(&config(&[dir.path(), &dir.path().join("missing")]));
    assert!(reason(&c, "a.json").contains("1.2.0"), "{c:?}");
    assert_eq!(c.unoffered.len(), 1, "a directory that is not there is not a fault");
}

#[test]
fn the_operators_flags_are_read_strictly() {
    assert_eq!(parse_class("korpis.test/standin=serial").unwrap().1, DeviceClass::Serial);
    assert!(parse_class("nvidia.com/gpu=pci").unwrap_err().contains("by its name"));
    assert!(parse_class("standin=serial").is_err());
    assert!(parse_class("a.com/x=disk").is_err());
    assert_eq!(parse_share("nvidia.com/gpu=4").unwrap(), ("nvidia.com/gpu".to_owned(), 4));
    assert!(parse_share("nvidia.com/gpu=0").is_err());
}
