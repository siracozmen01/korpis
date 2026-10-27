// SPDX-License-Identifier: Apache-2.0

//! Generates the Rust types and Connect service stubs from the committed descriptor set, which
//! scripts/gen-proto.sh builds from proto/. No protobuf compiler is needed here.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const FILES: &[&str] = &[
        "korpis/v1/common.proto",
        "korpis/v1/tenancy.proto",
        "korpis/v1/authority.proto",
        "korpis/v1/workload_spec.proto",
        "korpis/v1/recipe.proto",
        "korpis/v1/workload.proto",
        "korpis/v1/placement.proto",
        "korpis/v1/attached.proto",
        "korpis/v1/api.proto",
        "korpis/v1/event.proto",
        "korpis/v1/extension.proto",
        "korpis/v1/provider.proto",
        "korpis/v1/relay.proto",
        "korpis/v1/console.proto",
        "korpis/v1/files.proto",
        "korpis/v1/account.proto",
        "korpis/v1/session.proto",
        "korpis/v1/profile.proto",
        "korpis/v1/run.proto",
        "korpis/v1/metering.proto",
        "korpis/agent/v1/agent.proto",
        "korpis/agent/v1/data.proto",
        "korpis/relay/v1/relay.proto",
        "korpis/agent/v1/identity.proto",
        "korpis/agent/v1/meter.proto",
        "korpis/agent/v1/replica.proto",
        "korpis/driver/v1/driver.proto",
        "korpis/helper/v1/helper.proto",
        "korpis/backup/v1/backup.proto",
        "korpis/ingress/v1/ingress.proto",
        "korpis/install/v1/install.proto",
    ];
    println!("cargo:rerun-if-changed=descriptor.binpb");
    // Every map is a BTreeMap, so a message encodes to the same bytes every time: an
    // assignment is signed over its bytes and compared by their digest (§4.8 of
    // docs/02-architecture.md), and a HashMap's order would make every comparison a change.
    let mut buffa = connectrpc_build::CodeGenConfig::default();
    buffa.map_fields = vec![(".".to_owned(), buffa_codegen::MapRepr::BTreeMap)];
    connectrpc_build::Config::new()
        .buffa_config(buffa)
        .descriptor_set("descriptor.binpb")
        .files(FILES)
        .generate_json(true)
        .gate_client_feature(true)
        .include_file("_connectrpc.rs")
        .compile()?;
    // The service code is the only generated code that names connectrpc. Without the
    // `services` feature it is emptied, so the messages compile with no RPC stack linked.
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_SERVICES");
    if std::env::var_os("CARGO_FEATURE_SERVICES").is_none() {
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
        for entry in std::fs::read_dir(&out)? {
            let path = entry?.path();
            if path.to_string_lossy().ends_with(".__connect.rs") {
                std::fs::write(&path, "// services not built: the `services` feature is off\n")?;
            }
        }
    }
    Ok(())
}
