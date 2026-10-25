// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use super::*;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

/// The directory `dir`, owned by whoever runs the test, as a node's is by root.
fn config(dir: &Path) -> Config {
    let owner = std::fs::metadata(dir).unwrap().uid();
    Config { dirs: vec![dir.to_path_buf()], owner, ..Config::default() }
}

/// A file of kind `kind` with one device, `name`, giving `node`.
fn one(dir: &Path, kind: &str, name: &str, node: &str) {
    let text = format!(
        r#"{{"cdiVersion":"0.6.0","kind":"{kind}","annotations":{{"vendor":"x"}},
            "devices":[{{"name":"{name}",
                "containerEdits":{{"deviceNodes":[{{"path":"{node}"}}]}}}}]}}"#
    );
    std::fs::write(dir.join(format!("{name}.json")), text).unwrap();
}

fn reason<'a>(c: &'a Catalog, id: &str) -> &'a str {
    c.unoffered.iter().find(|u| u.id.ends_with(id)).map_or("", |u| u.reason.as_str())
}

#[test]
fn a_kind_named_for_a_class_is_offered_and_one_named_for_none_once_the_operator_maps_it() {
    let dir = tempfile::tempdir().unwrap();
    one(dir.path(), "nvidia.com/gpu", "0", "/dev/nvidia0");
    one(dir.path(), "korpis.test/standin", "a", "/dev/zero");
    let mut config = config(dir.path());
    let c = read(&config);
    assert_eq!(c.devices.len(), 1);
    assert_eq!(c.devices[0].class, pb::DeviceClass::DEVICE_CLASS_GPU);
    assert_eq!(c.devices[0].attributes.get("vendor").map(String::as_str), Some("x"));
    assert!(reason(&c, "standin=a").contains("unclassified"), "{c:?}");
    config.classes.insert("korpis.test/standin".into(), DeviceClass::Serial);
    config.shares.insert("korpis.test/standin".into(), 3);
    let c = read(&config);
    let standin = c.devices.iter().find(|d| d.id == "korpis.test/standin=a").unwrap();
    assert_eq!(standin.class, pb::DeviceClass::DEVICE_CLASS_SERIAL);
    assert_eq!(
        (standin.shares, standin.host_nodes.as_slice()),
        (3, ["/dev/zero".to_owned()].as_slice())
    );
    assert!(c.unoffered.is_empty(), "{c:?}");
}

#[test]
fn a_device_held_is_given_by_its_id_class_and_mode_and_one_gone_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    one(dir.path(), "nvidia.com/gpu", "0", "/dev/nvidia0");
    let c = read(&config(dir.path()));
    let hold = |device: &str| pb::DeviceHold {
        device: device.into(),
        mode: pb::DeviceMode::DEVICE_MODE_PASSTHROUGH.into(),
        ..Default::default()
    };
    let given = super::given(&c, &[hold("nvidia.com/gpu=0")]).unwrap();
    assert_eq!(given[0].id, "nvidia.com/gpu=0");
    assert_eq!(given[0].class, pb::DeviceClass::DEVICE_CLASS_GPU);
    assert_eq!(given[0].mode, pb::DeviceMode::DEVICE_MODE_PASSTHROUGH);
    let gone = super::given(&c, &[hold("nvidia.com/gpu=1")]).unwrap_err();
    assert!(gone.contains("no longer offered"), "{gone}");
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
