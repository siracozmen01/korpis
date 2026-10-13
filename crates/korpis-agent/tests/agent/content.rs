// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's content store against the development registry (§6 of docs/04-runtimes.md): an
//! image fetched by digest through its index, this machine's manifest chosen, every blob kept by
//! digest and checked; content named by a tag alone is refused.

use korpis_agent::content::{ContentStore, FetchError};
use korpis_core::content::{OCI_INDEX, OCI_MANIFEST, digest_of, this_machine};
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};

fn registry() -> String {
    std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned())
}

/// Pushes `bytes` as a blob of `repo`; its descriptor as JSON.
async fn blob(client: &Client, r: &Reference, media: &str, bytes: Vec<u8>) -> String {
    let digest = digest_of(&bytes);
    let size = bytes.len();
    client.push_blob(r, bytes, &digest).await.unwrap();
    format!(r#"{{"mediaType":"{media}","digest":"{digest}","size":{size}}}"#)
}

/// Pushes an image manifest with one layer of `text`; its digest and JSON.
async fn image(client: &Client, r: &Reference, text: &str) -> (String, String) {
    let layer_bytes = text.as_bytes().to_vec();
    let diff = digest_of(&layer_bytes);
    let layer = blob(client, r, "application/vnd.oci.image.layer.v1.tar", layer_bytes).await;
    let config_json = format!(r#"{{"rootfs":{{"type":"layers","diff_ids":["{diff}"]}}}}"#);
    let config =
        blob(client, r, "application/vnd.oci.image.config.v1+json", config_json.into()).await;
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"{OCI_MANIFEST}","config":{config},"layers":[{layer}]}}"#
    );
    let digest = digest_of(manifest.as_bytes());
    let at = r.clone_with_digest(digest.clone());
    client.push_manifest_raw(&at, manifest.clone(), OCI_MANIFEST.parse().unwrap()).await.unwrap();
    (digest, manifest)
}

/// A two-platform image; the index's reference, and the digest of this machine's manifest.
async fn push(repo: &str) -> (String, String) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let reg = registry();
    let client = Client::new(ClientConfig {
        protocol: ClientProtocol::HttpsExcept(vec![reg.clone()]),
        ..ClientConfig::default()
    });
    let r: Reference = format!("{reg}/{repo}:latest").parse().unwrap();
    client.auth(&r, &RegistryAuth::Anonymous, oci_client::RegistryOperation::Push).await.unwrap();
    let (mine, m) = image(&client, &r, &format!("this machine's layer of {repo}")).await;
    let (other, o) = image(&client, &r, &format!("another machine's layer of {repo}")).await;
    let (os, arch) = this_machine();
    let entry = |d: &str, size: usize, arch: &str| {
        format!(
            r#"{{"mediaType":"{OCI_MANIFEST}","digest":"{d}","size":{size},
                "platform":{{"os":"{os}","architecture":"{arch}"}}}}"#
        )
    };
    let index = format!(
        r#"{{"schemaVersion":2,"mediaType":"{OCI_INDEX}","manifests":[{},{}]}}"#,
        entry(&other, o.len(), "not-this-one"),
        entry(&mine, m.len(), arch)
    );
    let index_digest = digest_of(index.as_bytes());
    let at = r.clone_with_digest(index_digest.clone());
    client.push_manifest_raw(&at, index, OCI_INDEX.parse().unwrap()).await.unwrap();
    (format!("{reg}/{repo}@{index_digest}"), mine)
}

#[tokio::test]
async fn an_image_is_fetched_by_digest_for_this_machine_and_kept_by_digest() {
    let repo = format!("korpis-test/content-{}", uuid::Uuid::now_v7().simple());
    let (reference, mine) = push(&repo).await;
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::open(&dir.path().join("content"), vec![registry()]).unwrap();
    assert_eq!(store.fetch(&reference).await.unwrap(), mine, "this machine's manifest");
    let held = std::fs::read(store.path_of(&mine).unwrap()).unwrap();
    assert_eq!(digest_of(&held), mine, "kept under its digest");
    let blobs = std::fs::read_dir(dir.path().join("content/blobs/sha256")).unwrap().count();
    assert_eq!(blobs, 4, "the index, the manifest, the config, and the layer");
    assert_eq!(store.fetch(&reference).await.unwrap(), mine, "held content is not fetched again");

    let by_tag = format!("{}/{repo}:latest", registry());
    assert!(matches!(store.fetch(&by_tag).await, Err(FetchError::Reference(_))));
}
