// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider serving its store, reached by the client a node's job uses (`korpis-backup`'s)
//! with the access it grants, at the address the control plane would put in it: a job's
//! credential reaches its own prefix as its mode allows and nothing else, over TLS that trusts
//! the provider's certificate and no other; the provider's own listing and deletion answer as the
//! control plane addresses them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_backup::s3::{S3Config, S3Store};
use korpis_backup::store::ObjectStore as _;
use korpis_extension::backup::BackupTarget as _;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::object_access::Protocol;
use korpis_target_fs::serve::{Served, serve};
use korpis_target_fs::store::Store;
use korpis_target_fs::tokens::Tokens;
use korpis_target_fs::{FsTarget, tls};
use std::sync::Arc;
use std::time::Duration;

/// A provider over a fresh volume, serving on loopback; it and where it is served.
async fn provider(root: &std::path::Path) -> (FsTarget, String) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let store = Store::open(root).unwrap();
    let identity = tls::identity().unwrap();
    let tokens = Arc::new(Tokens::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = format!("https://{}", listener.local_addr().unwrap());
    let served = Served { store: store.clone(), tokens: tokens.clone() };
    tokio::spawn(serve(listener, identity.config, served));
    (FsTarget::new(store, tokens, "store".into(), identity.certificate), at)
}

/// A store reached with `access` at `at`, beneath `key_prefix`, trusting `certificate`.
fn client(access: &pb::ObjectAccess, at: &str, key_prefix: &str, certificate: &[u8]) -> S3Store {
    let Some(Protocol::S3(s)) = &access.protocol else { panic!("{access:?}") };
    S3Store::new(S3Config {
        endpoint: at.to_owned(),
        region: s.region.clone(),
        bucket: s.bucket.clone(),
        key_prefix: key_prefix.into(),
        access_key_id: s.access_key_id.clone(),
        secret_access_key: s.secret_access_key.clone(),
        session_token: s.session_token.clone(),
        path_style: s.path_style,
        certificate: certificate.to_owned(),
    })
    .unwrap()
}

fn certificate(access: &pb::ObjectAccess) -> Vec<u8> {
    let Some(Protocol::S3(s)) = &access.protocol else { panic!() };
    s.certificate.clone()
}

async fn granted(p: &FsTarget, prefix: &str, mode: pb::AccessMode) -> pb::ObjectAccess {
    p.grant_access(prefix.into(), mode, Duration::from_secs(900)).await.unwrap()
}

#[tokio::test]
async fn a_job_reaches_its_own_prefix_as_its_mode_allows_and_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    let (p, at) = provider(root.path()).await;
    let write = granted(&p, "workloads/w1/", pb::AccessMode::ACCESS_MODE_WRITE).await;
    let Some(Protocol::S3(s)) = &write.protocol else { panic!() };
    assert_eq!((s.served_by.as_str(), s.endpoint.as_str()), ("store", ""), "named, not addressed");
    assert_eq!(s.key_prefix, "workloads/w1/");
    let cert = certificate(&write);
    let own = client(&write, &at, "workloads/w1/", &cert);
    own.put("chunks/a", b"one".to_vec()).await.unwrap();
    own.put("chunks/a b+c", b"two".to_vec()).await.unwrap();
    assert_eq!(own.get("chunks/a").await.unwrap().unwrap(), b"one", "a backup reads its parent");
    assert_eq!(own.get("chunks/none").await.unwrap(), None);
    assert_eq!(own.list("chunks/").await.unwrap(), ["chunks/a", "chunks/a b+c"]);
    let other = client(&write, &at, "workloads/w2/", &cert);
    assert!(other.put("chunks/x", b"no".to_vec()).await.is_err(), "another workload's prefix");
    assert!(other.list("").await.is_err(), "nor its listing");

    let read = granted(&p, "workloads/w1/", pb::AccessMode::ACCESS_MODE_READ).await;
    let reader = client(&read, &at, "workloads/w1/", &cert);
    assert_eq!(reader.get("chunks/a b+c").await.unwrap().unwrap(), b"two");
    assert!(reader.put("chunks/c", b"no".to_vec()).await.is_err(), "a restore writes nothing");

    let stranger = tls::identity().unwrap().certificate;
    let fooled = client(&write, &at, "workloads/w1/", &stranger);
    assert!(fooled.get("chunks/a").await.is_err(), "a store presenting another certificate");
    let forged = pb::ObjectAccess { ..write.clone() };
    let Some(Protocol::S3(mut f)) = forged.protocol.clone() else { panic!() };
    f.secret_access_key = "not the secret".into();
    let forged = pb::ObjectAccess { protocol: Some(Protocol::S3(f)), ..forged };
    assert!(client(&forged, &at, "workloads/w1/", &cert).get("chunks/a").await.is_err());
}

#[tokio::test]
async fn a_prune_lists_page_by_page_and_deletes_on_the_volume() {
    let root = tempfile::tempdir().unwrap();
    let (p, at) = provider(root.path()).await;
    let write = granted(&p, "workloads/w1/", pb::AccessMode::ACCESS_MODE_WRITE).await;
    let own = client(&write, &at, "workloads/w1/", &certificate(&write));
    for name in ["backups/b1", "chunks/a", "chunks/b", "trees/t"] {
        own.put(name, b"x".to_vec()).await.unwrap();
    }
    let mut seen = Vec::new();
    let mut after = String::new();
    loop {
        let page = p.list("workloads/w1/".into(), after.clone(), 3).await.unwrap();
        seen.extend(page.objects.iter().map(|o| o.key.clone()));
        if !page.more {
            break;
        }
        after = seen.last().cloned().unwrap();
    }
    let expected =
        ["backups/b1", "chunks/a", "chunks/b", "trees/t"].map(|k| format!("workloads/w1/{k}"));
    assert_eq!(seen, expected);
    let gone = p.delete(vec![expected[1].clone(), "workloads/w1/none".into()]).await.unwrap();
    assert_eq!(gone, 1);
    assert_eq!(own.get("chunks/a").await.unwrap(), None);
    let refused =
        p.grant_access("../x/".into(), pb::AccessMode::ACCESS_MODE_READ, Duration::from_secs(60));
    assert!(refused.await.is_err(), "a prefix that is not plain");
}
