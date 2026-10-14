// SPDX-License-Identifier: AGPL-3.0-or-later

//! The S3 store against a real S3-compatible server, MinIO run as a host process: a volume backed
//! up, restored, and verified through it, beneath a prefix in a bucket; a listing longer than one
//! page; and a credential the store does not accept refused rather than retried.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

use korpis_backup::keys::KEY_LEN;
use korpis_backup::s3::{S3Config, S3Store};
use korpis_backup::store::{ObjectStore as _, StoreError};
use korpis_backup::testing::{Minio, SECRET};
use korpis_backup::{Content, Keys, NoContent, Options, Repo, Restore, backup, restore, verify};
use std::sync::Arc;

fn config(m: &Minio, prefix: &str, secret: &str) -> S3Config {
    S3Config { secret_access_key: secret.into(), ..m.access("backups", prefix) }
}

async fn minio() -> Option<Minio> {
    let m = Minio::start(std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))).await.unwrap()?;
    m.bucket("backups").await.unwrap();
    Some(m)
}

#[tokio::test]
async fn a_volume_is_backed_up_restored_and_verified_through_a_bucket() {
    let Some(m) = minio().await else { return };
    let store = Arc::new(S3Store::new(config(&m, "repo/workloads/wl_1/", SECRET)).unwrap());
    let r = Repo::new(Arc::clone(&store), Arc::new(Keys::derive(&[3; KEY_LEN])));
    let tmp = env!("CARGO_TARGET_TMPDIR");
    let (src, dst) = (tempfile::tempdir_in(tmp).unwrap(), tempfile::tempdir_in(tmp).unwrap());
    std::fs::create_dir(src.path().join("world")).unwrap();
    std::fs::write(src.path().join("world/level.dat"), vec![7u8; 3 << 20]).unwrap();
    std::fs::write(src.path().join("server.properties"), "motd=s3\n").unwrap();
    let options = Options {
        source: src.path().to_owned(),
        consistent: true,
        parent: None,
        content: Content::default(),
        concurrency: 8,
    };
    backup(&r, "b1", options).await.unwrap();
    let what = Restore {
        backup: "b1".into(),
        path: String::new(),
        target: dst.path().to_owned(),
        content: Arc::new(NoContent),
        concurrency: 8,
    };
    restore(&r, what).await.unwrap();
    verify(&r, "b1", "", dst.path().to_owned()).await.unwrap();

    let outside = S3Store::new(config(&m, "repo/workloads/wl_2/", SECRET)).unwrap();
    assert!(outside.list("").await.unwrap().is_empty(), "another prefix sees nothing of it");
    assert_eq!(store.get("backups/b9").await.unwrap(), None);
}

#[tokio::test]
async fn a_listing_goes_past_one_page() {
    let Some(m) = minio().await else { return };
    let store = Arc::new(S3Store::new(config(&m, "p/", SECRET)).unwrap());
    let puts = (0..1100).map(|i| {
        let s = Arc::clone(&store);
        async move { s.put(&format!("chunks/{i:05}"), vec![1]).await }
    });
    futures_util::future::try_join_all(puts).await.unwrap();
    let names = store.list("chunks/").await.unwrap();
    assert_eq!(names.len(), 1100);
    assert_eq!(names.first().map(String::as_str), Some("chunks/00000"));
}

#[tokio::test]
async fn a_credential_the_store_does_not_accept_is_refused() {
    let Some(m) = minio().await else { return };
    let wrong = S3Store::new(config(&m, "p/", "not-the-secret")).unwrap();
    let e = wrong.put("chunks/a", vec![1]).await.unwrap_err();
    assert!(matches!(&e, StoreError::Refused(why) if why.contains("403")), "{e}");
    assert!(S3Store::new(config(&m, "no-slash", SECRET)).is_err());
    let mut far = config(&m, "p/", SECRET);
    far.endpoint = "http://s3.example".into();
    assert!(S3Store::new(far).is_err(), "plain HTTP only to this machine");
    let store = S3Store::new(config(&m, "p/", SECRET)).unwrap();
    assert!(store.put("../escape", vec![1]).await.is_err());
}
