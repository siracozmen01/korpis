// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider against a real S3-compatible store (MinIO, `scripts/test-tools.sh`; skipped
//! without it): a job's credential reaches its own prefix and nothing else, writes and reads for
//! a backup, only reads for a restore, deletes nothing; the provider's own listing and deletion
//! answer as the control plane addresses them; and nothing longer than asked, nor a prefix that a
//! policy would read as a pattern, is granted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_backup::s3::{S3Config, S3Store};
use korpis_backup::store::ObjectStore as _;
use korpis_backup::testing::{Minio, SECRET, USER};
use korpis_extension::backup::BackupTarget as _;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::object_access::Protocol;
use korpis_target_s3::sts::Sts;
use korpis_target_s3::{Place, S3Target};
use std::time::Duration;

fn provider(m: &Minio) -> S3Target {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let endpoint: url::Url = m.endpoint.parse().unwrap();
    let sts = Sts {
        endpoint: endpoint.clone(),
        region: "us-east-1".into(),
        access_key_id: USER.into(),
        secret: SECRET.into(),
        role_arn: None,
        http: reqwest::Client::new(),
    };
    let place = Place {
        endpoint,
        region: "us-east-1".into(),
        bucket: "backups".into(),
        root: "korpis/".into(),
        path_style: true,
        access_key_id: USER.into(),
        secret: SECRET.into(),
    };
    S3Target::new(place, sts).unwrap()
}

/// A store reached with `access`, beneath `key_prefix` (its own, unless a test reaches further).
fn store(access: &pb::ObjectAccess, key_prefix: &str) -> S3Store {
    let Some(Protocol::S3(s)) = &access.protocol else { panic!("{access:?}") };
    S3Store::new(S3Config {
        endpoint: s.endpoint.clone(),
        region: s.region.clone(),
        bucket: s.bucket.clone(),
        key_prefix: key_prefix.into(),
        access_key_id: s.access_key_id.clone(),
        secret_access_key: s.secret_access_key.clone(),
        session_token: s.session_token.clone(),
        path_style: s.path_style,
        certificate: s.certificate.clone(),
    })
    .unwrap()
}

async fn granted(p: &S3Target, prefix: &str, mode: pb::AccessMode) -> pb::ObjectAccess {
    p.grant_access(prefix.into(), mode, Duration::from_secs(900)).await.unwrap()
}

#[tokio::test]
async fn a_job_reaches_its_own_prefix_as_its_mode_allows_and_nothing_else() {
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    minio.bucket("backups").await.unwrap();
    let p = provider(&minio);

    let write = granted(&p, "workloads/w1/", pb::AccessMode::ACCESS_MODE_WRITE).await;
    let Some(Protocol::S3(s)) = &write.protocol else { panic!() };
    assert_eq!(s.key_prefix, "korpis/workloads/w1/");
    assert_ne!(s.access_key_id, USER, "a temporary credential, never the provider's own");
    assert!(write.expires_at.as_option().is_some());
    let own = store(&write, "korpis/workloads/w1/");
    own.put("chunks/a", b"one".to_vec()).await.unwrap();
    own.put("chunks/b", b"two".to_vec()).await.unwrap();
    assert_eq!(own.get("chunks/a").await.unwrap().unwrap(), b"one", "a backup reads its parent");
    assert_eq!(own.list("chunks/").await.unwrap(), ["chunks/a", "chunks/b"]);
    let other = store(&write, "korpis/workloads/w2/");
    assert!(other.put("chunks/x", b"no".to_vec()).await.is_err(), "another workload's prefix");
    assert!(other.list("").await.is_err(), "nor its listing");
    let wider = store(&write, "korpis/");
    assert!(wider.get("workloads/w1/chunks/a").await.is_ok(), "beneath its prefix, still");
    assert!(wider.put("elsewhere", b"no".to_vec()).await.is_err());

    let read = granted(&p, "workloads/w1/", pb::AccessMode::ACCESS_MODE_READ).await;
    let reader = store(&read, "korpis/workloads/w1/");
    assert_eq!(reader.get("chunks/b").await.unwrap().unwrap(), b"two");
    assert!(reader.put("chunks/c", b"no".to_vec()).await.is_err(), "a restore writes nothing");
    assert!(minio_delete(&write, "korpis/workloads/w1/chunks/a").await >= 400, "nor deletes");
}

/// The status of deleting `key` with `access` directly.
async fn minio_delete(access: &pb::ObjectAccess, key: &str) -> u16 {
    use rusty_s3::actions::S3Action as _;
    let Some(Protocol::S3(s)) = &access.protocol else { panic!() };
    let bucket = rusty_s3::Bucket::new(
        s.endpoint.parse().unwrap(),
        rusty_s3::UrlStyle::Path,
        s.bucket.clone(),
        s.region.clone(),
    )
    .unwrap();
    let creds = rusty_s3::Credentials::new_with_token(
        s.access_key_id.clone(),
        s.secret_access_key.clone(),
        s.session_token.clone(),
    );
    let url = bucket.delete_object(Some(&creds), key).sign(Duration::from_secs(60));
    reqwest::Client::new().delete(url).send().await.unwrap().status().as_u16()
}

#[tokio::test]
async fn a_prune_lists_page_by_page_and_deletes_with_the_provider_s_own_credential() {
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    minio.bucket("backups").await.unwrap();
    let p = provider(&minio);
    let write = granted(&p, "workloads/w1/", pb::AccessMode::ACCESS_MODE_WRITE).await;
    let own = store(&write, "korpis/workloads/w1/");
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
        after = seen.last().unwrap().clone();
    }
    assert_eq!(
        seen,
        [
            "workloads/w1/backups/b1",
            "workloads/w1/chunks/a",
            "workloads/w1/chunks/b",
            "workloads/w1/trees/t"
        ]
    );
    let gone = p.delete(vec!["workloads/w1/chunks/a".into(), "workloads/w1/trees/t".into()]).await;
    assert_eq!(gone.unwrap(), 2);
    let left = p.list("workloads/w1/".into(), String::new(), 1000).await.unwrap();
    let left: Vec<_> = left.objects.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(left, ["workloads/w1/backups/b1", "workloads/w1/chunks/b"]);
    assert!(p.delete(vec!["../x".into()]).await.is_err());
}

#[tokio::test]
async fn nothing_longer_than_asked_nor_a_pattern_is_granted() {
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    let p = provider(&minio);
    let w = pb::AccessMode::ACCESS_MODE_WRITE;
    let short = p.grant_access("workloads/w1/".into(), w, Duration::from_secs(60)).await;
    assert_eq!(short.unwrap_err().reason, "access.ttl");
    for bad in ["workloads/*/", "${aws:username}/", "workloads/../"] {
        let e = p.grant_access(bad.into(), w, Duration::from_secs(900)).await.unwrap_err();
        assert_eq!(e.reason, "access.prefix", "{bad}");
    }
}
