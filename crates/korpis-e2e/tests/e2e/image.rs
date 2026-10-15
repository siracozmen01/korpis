// SPDX-License-Identifier: AGPL-3.0-or-later

//! The test extension (`korpis-extension`'s example), built static and pushed to the test
//! registry as an OCI image of one file, so a real node runs it in a container.

use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Where the extension is in its image.
pub const ENTRY: &str = "/extension";

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Builds the test extension once, linked static so the image needs nothing beside it, into a
/// target directory of its own, since its flags are not the workspace's.
fn build() -> PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let triple = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
            let target = root.join("target/static");
            let flags =
                ["-C", "target-feature=+crt-static", "-C", "debuginfo=0", "-C", "strip=symbols"];
            let built = Command::new(env!("CARGO"))
                .current_dir(&root)
                .args(["build", "-q", "--locked", "-p", "korpis-extension", "--example"])
                .args(["test-extension", "--target", &triple, "--target-dir"])
                .arg(&target)
                .env_remove("RUSTFLAGS")
                .env("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f"))
                .status()
                .unwrap();
            assert!(built.success(), "the test extension did not build");
            target.join(triple).join("debug/examples/test-extension")
        })
        .clone()
}

/// A tar of `binary` alone, at [`ENTRY`].
fn layer(binary: &[u8]) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(binary.len()).unwrap());
    header.set_mode(0o755);
    header.set_cksum();
    tar.append_data(&mut header, ENTRY.trim_start_matches('/'), binary).unwrap();
    tar.into_inner().unwrap()
}

/// Uploads `blob` to `repo` at `registry`.
async fn upload(http: &reqwest::Client, (registry, repo): (&str, &str), blob: &[u8]) {
    let start = format!("http://{registry}/v2/{repo}/blobs/uploads/");
    let begun = http.post(start).send().await.unwrap();
    assert!(begun.status().is_success(), "{}", begun.status());
    let location = begun.headers()["location"].to_str().unwrap().to_owned();
    let location =
        if location.starts_with('/') { format!("http://{registry}{location}") } else { location };
    let sep = if location.contains('?') { '&' } else { '?' };
    let put = format!("{location}{sep}digest={}", digest(blob));
    let done = http.put(put).body(blob.to_vec()).send().await.unwrap();
    assert!(done.status().is_success(), "{}", done.status());
}

/// Pushes the test extension's image to a repository of its own; its reference by digest.
pub async fn push_extension_image(registry: &str) -> String {
    let binary = tokio::task::spawn_blocking(build).await.unwrap();
    let layer = layer(&std::fs::read(binary).unwrap());
    let arch = if std::env::consts::ARCH == "aarch64" { "arm64" } else { "amd64" };
    let config = serde_json::json!({
        "architecture": arch, "os": "linux",
        "config": {"Entrypoint": [ENTRY]},
        "rootfs": {"type": "layers", "diff_ids": [digest(&layer)]},
    });
    let config = serde_json::to_vec(&config).unwrap();
    let repo = format!("test/extension-image-{}", uuid::Uuid::now_v7().simple());
    let http = reqwest::Client::new();
    upload(&http, (registry, &repo), &config).await;
    upload(&http, (registry, &repo), &layer).await;
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                   "digest": digest(&config), "size": config.len()},
        "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar",
                    "digest": digest(&layer), "size": layer.len()}],
    });
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let put = http
        .put(format!("http://{registry}/v2/{repo}/manifests/1.0.0"))
        .header("content-type", "application/vnd.oci.image.manifest.v1+json")
        .body(manifest.clone())
        .send()
        .await
        .unwrap();
    assert!(put.status().is_success(), "{}", put.status());
    format!("{registry}/{repo}@{}", digest(&manifest))
}
