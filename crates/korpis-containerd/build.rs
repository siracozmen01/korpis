// SPDX-License-Identifier: Apache-2.0

//! Generates containerd's API from the committed descriptor set, which scripts/gen-proto.sh
//! builds from proto/ with the pinned buf. No protobuf compiler is needed here.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const FILES: &[&str] = &[
        "types/descriptor.proto",
        "types/metrics.proto",
        "types/mount.proto",
        "types/platform.proto",
        "types/task/task.proto",
        "types/transfer/imagestore.proto",
        "types/transfer/progress.proto",
        "types/transfer/registry.proto",
        "services/containers/v1/containers.proto",
        "services/content/v1/content.proto",
        "services/diff/v1/diff.proto",
        "services/images/v1/images.proto",
        "services/leases/v1/leases.proto",
        "services/namespaces/v1/namespace.proto",
        "services/snapshots/v1/snapshots.proto",
        "services/tasks/v1/tasks.proto",
        "services/transfer/v1/transfer.proto",
        "services/version/v1/version.proto",
    ];
    println!("cargo:rerun-if-changed=descriptor.binpb");
    // Ordered maps, as in korpis-proto: a label set encodes the same way every time.
    let mut buffa = connectrpc_build::CodeGenConfig::default();
    buffa.map_fields = vec![(".".to_owned(), buffa_codegen::MapRepr::BTreeMap)];
    connectrpc_build::Config::new()
        .buffa_config(buffa)
        .descriptor_set("descriptor.binpb")
        .files(FILES)
        .generate_json(true)
        .include_file("_connectrpc.rs")
        .compile()?;
    Ok(())
}
