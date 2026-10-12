// SPDX-License-Identifier: AGPL-3.0-or-later

//! The document format: what loads, and what does not.

use crate::support::PAPER;
use korpis_core::recipe::install::InstallStep;
use korpis_core::recipe::template::TemplateFormat;
use korpis_core::recipe::{ConfigDelivery, FieldPermission};
use korpis_recipe::{DocumentError, parse};

fn refused(from: &str, to: &str) -> DocumentError {
    assert!(PAPER.contains(from), "the fixture contains {from:?}");
    parse(PAPER.replacen(from, to, 1).as_bytes()).expect_err("refused")
}

fn field_of(e: &DocumentError) -> String {
    match e {
        DocumentError::Invalid(r) => r.field.clone(),
        DocumentError::Syntax(m) => panic!("expected a rule violation, got syntax: {m}"),
    }
}

#[test]
fn the_documented_example_loads_whole() {
    let r = parse(PAPER.as_bytes()).unwrap();
    assert_eq!((r.metadata.name.as_str(), r.metadata.version.as_str()), ("paper", "1.21.4"));
    assert_eq!(r.runtime.targets.len(), 2);
    assert_eq!(r.preset.endpoints.len(), 3);
    assert_eq!(r.preset.volumes[0].size_default_bytes, 10 << 30);
    assert_eq!(r.preset.resources.cpu_min_millis, 500);
    let eula = r.config.field("eula").unwrap();
    assert_eq!(eula.permission, FieldPermission::System, "a constant is system-set");
    assert_eq!(r.config.field("rcon_password").unwrap().deliver, ConfigDelivery::Env);
    assert!(matches!(r.install[0], InstallStep::Fetch { .. }));
    assert!(matches!(r.install[1], InstallStep::Mkdir { mode: 0o755, .. }));
    assert_eq!(r.templates[0].format, TemplateFormat::Properties);
    assert!(r.lifecycle_hooks.pre_stop.is_some());
    assert_eq!(r.disruption.disruptive_fields, ["max_players", "difficulty"]);
    let mut paths = r.artifact_paths();
    paths.sort_unstable();
    assert_eq!(paths, ["./layers/rootfs.tar.zst", "templates/server.properties"]);
}

#[test]
fn installs_run_no_code_and_fetch_nothing_unhashed() {
    let no_hash = refused(
        r#"sha256: "1111111111111111111111111111111111111111111111111111111111111111", "#,
        "",
    );
    assert_eq!(field_of(&no_hash), "install.steps[0].fetch.sha256");
    let run = refused(
        "- mkdir: {path: /data/plugins, mode: \"0755\"}",
        "- run: {command: \"curl x | sh\"}",
    );
    assert_eq!(field_of(&run), "install.steps[1]");
    let escape = refused("write: {path: /data/eula.txt", "write: {path: /data/../etc/passwd");
    assert_eq!(field_of(&escape), "install.steps[2].path");
}

#[test]
fn ownership_is_workload_relative() {
    let chown = |uid: u32| {
        PAPER.replacen(
            "- write: {path: /data/eula.txt",
            &format!(
                "- chown: {{path: /data, uid: {uid}, gid: 0}}\n    - write: {{path: /data/eula.txt"
            ),
            1,
        )
    };
    parse(chown(65_535).as_bytes()).unwrap();
    let host = parse(chown(100_000).as_bytes()).unwrap_err();
    assert_eq!(field_of(&host), "install.steps[2]");
}

#[test]
fn protected_values_stay_out_of_tenant_files() {
    let e = refused("vars: [motd, max_players, difficulty]", "vars: [motd, rcon_password]");
    assert_eq!(field_of(&e), "templates[0]");
    let e = refused("deliver: env, deliver_as: RCON_PASSWORD", "deliver: file");
    assert_eq!(field_of(&e), "config.rcon_password");
}

#[test]
fn typos_are_errors_with_a_place_not_silence() {
    let e = refused("working_dir: /data", "workdir: /data");
    let DocumentError::Syntax(m) = e else { panic!("a syntax error") };
    assert!(m.contains("workdir"), "{m}");
    assert!(m.contains("line") || m.contains(':'), "the message locates it: {m}");
    assert!(matches!(refused("kind: Recipe", "kind: Egg"), DocumentError::Invalid(_)));
    let probe =
        refused("readiness: {kind: tcp, endpoint: game", "readiness: {kind: tcp, endpoint: gaem");
    assert_eq!(field_of(&probe), "probes.health.readiness");
    assert!(parse(&vec![b' '; korpis_recipe::document::MAX_DOCUMENT + 1]).is_err());
}
