// SPDX-License-Identifier: Apache-2.0

//! Generates the Rust types and Connect service stubs from the committed descriptor set, which
//! scripts/gen-proto.sh builds from proto/. No protobuf compiler is needed here.

fn main() {
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
    connectrpc_build::Config::new()
        .descriptor_set("descriptor.binpb")
        .files(FILES)
        .generate_json(true)
        .gate_client_feature(true)
        .include_file("_connectrpc.rs")
        .compile()
        .expect("generate code from descriptor.binpb");
}
