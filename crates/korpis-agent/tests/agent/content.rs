// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's content store against the development registry (§6 of docs/04-runtimes.md): an
//! image fetched by digest through its index, this machine's manifest chosen, every blob kept by
//! digest and checked; content named by a tag alone is refused.

use korpis_agent::content::{ContentStore, FetchError, Reach};
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

/// Serves `body` to every request for `/file` on loopback, and 404 otherwise; the URL, and how
/// many requests were answered.
async fn serving(body: &'static [u8]) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = std::sync::Arc::clone(&count);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            counting.fetch_add(1, Ordering::SeqCst);
            let mut request = [0u8; 2048];
            let n = s.read(&mut request).await.unwrap_or(0);
            let asked = String::from_utf8_lossy(&request[..n]).into_owned();
            let found = asked.starts_with("GET /file ");
            let head = if asked.starts_with("GET /hop ") {
                // A redirect to the same server by an address the test does not allow.
                format!(
                    "HTTP/1.1 302 Found\r\nlocation: http://[::1]:{}/file\r\n\
                     content-length: 0\r\nconnection: close\r\n\r\n",
                    at.port()
                )
            } else if found {
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                )
            } else {
                "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    .to_owned()
            };
            let _ = s.write_all(head.as_bytes()).await;
            if found {
                let _ = s.write_all(body).await;
            }
        }
    });
    (format!("http://{at}/file"), count)
}

fn sha(bytes: &[u8], wide: bool) -> String {
    use sha2::Digest as _;
    let hex = |b: &[u8]| {
        b.iter().fold(String::new(), |mut s, x| {
            let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{x:02x}"));
            s
        })
    };
    if wide {
        format!("sha512:{}", hex(&sha2::Sha512::digest(bytes)))
    } else {
        format!("sha256:{}", hex(&sha2::Sha256::digest(bytes)))
    }
}

#[tokio::test]
async fn a_fetch_is_kept_only_when_it_hashes_to_what_was_declared_and_fetched_once() {
    use std::sync::atomic::Ordering;
    const BODY: &[u8] = b"a server jar, as it were";
    let (url, count) = serving(BODY).await;
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::open(&dir.path().join("content"), Vec::new())
        .unwrap()
        .reaching(loopback_v4())
        .unwrap();

    let wrong = sha(b"something else", false);
    let e = store.fetch_url(&url, &wrong).await.unwrap_err();
    assert!(matches!(e, FetchError::Mismatch(_)), "{e:?}");
    assert!(!store.file_of(&wrong).unwrap().exists(), "nothing kept under the declared hash");
    let leftovers = std::fs::read_dir(dir.path().join("content/ingest")).unwrap().count();
    assert_eq!(leftovers, 0, "and nothing left half-kept");

    for wide in [false, true] {
        let digest = sha(BODY, wide);
        let path = store.fetch_url(&url, &digest).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), BODY);
        let before = count.load(Ordering::SeqCst);
        assert_eq!(store.fetch_url(&url, &digest).await.unwrap(), path);
        assert_eq!(count.load(Ordering::SeqCst), before, "held: not fetched again");
    }
    let missing = url.replace("/file", "/gone");
    assert!(store.fetch_url(&missing, &sha(b"x", false)).await.is_err(), "a 404 is a failure");
    let e = store.fetch_url("ftp://example.org/x", &sha(b"x", false)).await.unwrap_err();
    assert!(matches!(e, FetchError::Reference(_)), "only http and https: {e:?}");
    assert!(store.file_of("sha256:NOTHEX").is_err());
}

#[tokio::test]
async fn a_recipe_s_files_are_fetched_with_their_paths_and_kept_by_digest() {
    let repo = crate::support::push_paper().await;
    let registry = registry();
    let tags = korpis_recipe::registry::Registry::new(vec![registry.clone()]).unwrap();
    let digest = tags.resolve(&repo, "1.21.4").await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::open(&dir.path().join("content"), vec![registry]).unwrap();
    let files = store.fetch_files(&format!("{repo}@{digest}")).await.unwrap();
    let paths: Vec<&str> = files.keys().map(String::as_str).collect();
    assert_eq!(paths, ["layers/rootfs.tar.zst", "templates/server.properties"]);
    let template = &files["templates/server.properties"];
    assert_eq!(std::fs::read(store.path_of(template).unwrap()).unwrap(), b"motd={{motd}}\n");
}

/// What the tests' servers are on, which a node's fetch reaches only when the operator says so.
fn loopback_v4() -> Reach {
    Reach::with(vec!["127.0.0.1/32".parse().unwrap()])
}

#[tokio::test]
async fn a_fetch_never_reaches_the_node_s_own_or_private_networks_unless_the_operator_allows() {
    use std::sync::atomic::Ordering;
    const BODY: &[u8] = b"anything";
    let (url, count) = serving(BODY).await;
    let digest = sha(BODY, false);
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::open(&dir.path().join("content"), Vec::new()).unwrap();
    let e = store.fetch_url(&url, &digest).await.unwrap_err();
    assert!(matches!(e, FetchError::Refused(_)), "an address in the URL: {e:?}");
    let named = url.replace("127.0.0.1", "localhost");
    assert!(store.fetch_url(&named, &digest).await.is_err(), "a name resolving to loopback");
    for refused in ["http://169.254.169.254/latest/meta-data", "http://[::ffff:10.0.0.1]/x"] {
        let e = store.fetch_url(refused, &digest).await.unwrap_err();
        assert!(matches!(e, FetchError::Refused(_)), "{refused}: {e:?}");
    }
    assert_eq!(count.load(Ordering::SeqCst), 0, "no request was sent to any of them");

    let allowed = ContentStore::open(&dir.path().join("allowed"), Vec::new())
        .unwrap()
        .reaching(loopback_v4())
        .unwrap();
    let hop = url.replace("/file", "/hop");
    assert!(allowed.fetch_url(&hop, &digest).await.is_err(), "a redirect is checked too");
    assert_eq!(count.load(Ordering::SeqCst), 1, "and is not followed");
    allowed.fetch_url(&url, &digest).await.unwrap();
}

#[tokio::test]
async fn an_image_from_a_registry_on_the_node_s_own_network_is_pulled_only_when_allowed() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap();
    let connections = std::sync::Arc::new(AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&connections);
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            drop(s);
        }
    });
    let image = format!("{at}/internal/thing@{}", sha(b"x", false));
    let dir = tempfile::tempdir().unwrap();
    let store = ContentStore::open(&dir.path().join("content"), Vec::new()).unwrap();
    assert!(store.fetch(&image).await.is_err());
    assert_eq!(connections.load(Ordering::SeqCst), 0, "nothing reached it");
    let allowed = store.reaching(loopback_v4()).unwrap();
    assert!(allowed.fetch(&image).await.is_err(), "it is no registry");
    assert!(connections.load(Ordering::SeqCst) > 0, "but it was reached");
}
