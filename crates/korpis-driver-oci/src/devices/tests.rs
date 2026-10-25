// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use super::*;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

/// A spec file in `dir` of kind `korpis.test/standin` whose device `name` has `edits`.
fn spec_file(dir: &Path, name: &str, edits: &str) {
    let text = format!(
        r#"{{"cdiVersion":"1.1.0","kind":"korpis.test/standin",
            "devices":[{{"name":"{name}","containerEdits":{edits}}}]}}"#
    );
    std::fs::write(dir.join(format!("{name}.json")), text).unwrap();
}

/// The files of `dir`, owned by whoever runs the test, as a node's are by root.
fn specs(dir: &Path) -> Specs {
    Specs { dirs: vec![dir.to_path_buf()], owner: std::fs::metadata(dir).unwrap().uid() }
}

/// A spec asking for `ids` in `passthrough`.
fn asking(ids: &[&str]) -> driver::PreparedSpec {
    let devices = ids
        .iter()
        .map(|id| driver::GivenDevice {
            id: (*id).into(),
            class: pb::DeviceClass::DEVICE_CLASS_SERIAL.into(),
            mode: pb::DeviceMode::DEVICE_MODE_PASSTHROUGH.into(),
            ..Default::default()
        })
        .collect();
    driver::PreparedSpec { devices, ..Default::default() }
}

#[test]
fn a_described_device_is_given_its_files_edits_with_its_node_as_the_host_has_it() {
    let dir = tempfile::tempdir().unwrap();
    let edits = r#"{"deviceNodes":[{"path":"/dev/standin","hostPath":"/dev/null"}],
        "env":["STANDIN=1"],"additionalGIDs":[44],
        "mounts":[{"hostPath":"/etc/hostname","containerPath":"/etc/standin",
                   "options":["ro","nosuid","nodev","rbind"]}]}"#;
    spec_file(dir.path(), "a", edits);
    let given = given(&asking(&["korpis.test/standin=a"]), &specs(dir.path())).unwrap();
    let null = Bound {
        host: "/dev/null".into(),
        path: "/dev/standin".into(),
        numbers: (1, 3),
        access: "rwm".into(),
    };
    assert_eq!(given.nodes, [null]);
    assert_eq!((given.env, given.gids), (vec!["STANDIN=1".to_owned()], vec![44]));
    assert_eq!(given.mounts[0].container_path, "/etc/standin");
}

#[test]
fn a_device_the_files_do_not_describe_or_whose_edits_are_not_applied_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let specs = specs(dir.path());
    let refused = |ids: &[&str]| given(&asking(ids), &specs).unwrap_err();
    assert!(refused(&["korpis.test/standin=gone"]).contains("not described"));
    spec_file(dir.path(), "rdt", r#"{"deviceNodes":[{"path":"/dev/null"}],"intelRdt":{}}"#);
    assert!(refused(&["korpis.test/standin=rdt"]).contains("Intel RDT"));
    let moved = r#"{"deviceNodes":[{"path":"/dev/null"}],
        "netDevices":[{"hostInterfaceName":"eth9","name":"eth0"}]}"#;
    spec_file(dir.path(), "net", moved);
    assert!(refused(&["korpis.test/standin=net"]).contains("network interface"));
    spec_file(dir.path(), "mode", r#"{"deviceNodes":[{"path":"/dev/null","fileMode":438}]}"#);
    assert!(refused(&["korpis.test/standin=mode"]).contains("mode or owner"));
    let file = dir.path().join("plain");
    std::fs::write(&file, "").unwrap();
    let plain = format!(r#"{{"deviceNodes":[{{"path":"{}"}}]}}"#, file.display());
    spec_file(dir.path(), "plain", &plain);
    assert!(refused(&["korpis.test/standin=plain"]).contains("not a character device"));
    spec_file(dir.path(), "numbers", r#"{"deviceNodes":[{"path":"/dev/null","minor":5}]}"#);
    assert!(refused(&["korpis.test/standin=numbers"]).contains("other numbers"));
}

#[test]
fn the_devices_named_are_cdi_names_given_once_in_a_mode_this_driver_gives() {
    assert!(lexical(&asking(&["korpis.test/standin=a"])).is_ok());
    assert!(lexical(&asking(&["standin"])).is_err());
    assert!(lexical(&asking(&["korpis.test/standin=a", "korpis.test/standin=a"])).is_err());
    let mut mediated = asking(&["korpis.test/standin=a"]);
    mediated.devices[0].mode = pb::DeviceMode::DEVICE_MODE_MEDIATED.into();
    assert!(lexical(&mediated).is_err());
}
