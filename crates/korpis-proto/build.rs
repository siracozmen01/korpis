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
        "korpis/agent/v1/agent.proto",
        "korpis/driver/v1/driver.proto",
        "korpis/helper/v1/helper.proto",
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
    Ok(())
}
