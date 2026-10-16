// SPDX-License-Identifier: AGPL-3.0-or-later

//! Programs of the workspace, built static and pushed to the test registry as OCI images of one
//! file each, so a real node runs them in containers: the test extension (`korpis-extension`'s
//! example), the test web program (this crate's), and the first-party extensions.

use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};

/// A program of the workspace.
#[derive(Debug, Clone, Copy)]
pub struct Program {
    /// Its package.
    pub package: &'static str,
    /// Whether it is an example rather than a binary.
    pub example: bool,
    /// Its name, which is also its path in its image, at the root.
    pub name: &'static str,
}

/// The test extension.
pub const TEST_EXTENSION: Program =
    Program { package: "korpis-extension", example: true, name: "test-extension" };
/// The test web program: HTTP on 8080, answering with the request's head, and an echo on 8443.
pub const TEST_WEB: Program = Program { package: "korpis-e2e", example: true, name: "test-web" };
/// The backup target for a disk.
pub const FS_TARGET: Program =
    Program { package: "korpis-target-fs", example: false, name: "korpis-target-fs" };

impl Program {
    /// Where it is in its image.
    pub fn entry(self) -> String {
        format!("/{}", self.name)
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Builds `p` once per run, linked static so its image needs nothing beside it, into a target
/// directory of its own, since its flags are not the workspace's.
fn build(p: Program) -> PathBuf {
    static BUILT: Mutex<Option<HashMap<&'static str, PathBuf>>> = Mutex::new(None);
    let mut built = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(path) = built.as_ref().and_then(|b| b.get(p.name)) {
        return path.clone();
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let triple = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
    let target = root.join("target/static");
    let flags = ["-C", "target-feature=+crt-static", "-C", "debuginfo=0", "-C", "strip=symbols"];
    let kind = if p.example { "--example" } else { "--bin" };
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["build", "-q", "--locked", "-p", p.package, kind, p.name, "--target", &triple])
        .arg("--target-dir")
        .arg(&target)
        .env_remove("RUSTFLAGS")
        .env("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f"))
        .status()
        .unwrap();
    assert!(status.success(), "{} did not build", p.name);
    let dir = target.join(triple).join("debug");
    let path = if p.example { dir.join("examples").join(p.name) } else { dir.join(p.name) };
    built.get_or_insert_default().insert(p.name, path.clone());
    path
}

/// A tar of `binary` alone, at `entry`.
fn layer(binary: &[u8], entry: &str) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(binary.len()).unwrap());
    header.set_mode(0o755);
    header.set_cksum();
    tar.append_data(&mut header, entry.trim_start_matches('/'), binary).unwrap();
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

/// Pushes `p`'s image to a repository of its own; its reference by digest.
pub async fn push_image(registry: &str, p: Program) -> String {
    let binary = tokio::task::spawn_blocking(move || build(p)).await.unwrap();
    let layer = layer(&std::fs::read(binary).unwrap(), &p.entry());
    let arch = if std::env::consts::ARCH == "aarch64" { "arm64" } else { "amd64" };
    let config = serde_json::json!({
        "architecture": arch, "os": "linux",
        "config": {"Entrypoint": [p.entry()]},
        "rootfs": {"type": "layers", "diff_ids": [digest(&layer)]},
    });
    let config = serde_json::to_vec(&config).unwrap();
    let repo = format!("test/{}-{}", p.name, uuid::Uuid::now_v7().simple());
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
