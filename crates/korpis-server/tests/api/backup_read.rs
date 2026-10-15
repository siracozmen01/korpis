// SPDX-License-Identifier: AGPL-3.0-or-later

//! Browsing and reading a backup through the control plane (§5.4 of docs/06-storage.md), against
//! a real object store: a backup the job wrote with the key and access the control plane granted
//! it is browsed directory by directory and one file read byte for byte; `backup.browse` and
//! `backup.read` each allow their own read and not the other's; each read is recorded. It needs
//! MinIO (`scripts/test-tools.sh`), and is skipped without it.

use crate::backup_target::serving_with;
use crate::backups::{access, bare, create, ended, repository};
use crate::journal::{ack, assignment, complete, send, until};
use crate::snapshots::{running, snapshotting};
use crate::support::Harness;
use crate::volumes::holding;
use connectrpc::ErrorCode;
use korpis_backup::keys::Keys;
use korpis_backup::s3::{S3Config, S3Store};
use korpis_backup::testing::{Minio, SECRET, USER};
use korpis_backup::{Content, Options, Repo, backup};
use korpis_extension::backup::{BackupTarget, Failure};
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::object_access::Protocol;
use std::sync::Arc;
use std::time::Duration;

/// A backup target granting access to a MinIO bucket, beneath the prefix asked for.
struct Bucket(String);

impl BackupTarget for Bucket {
    fn grant_access(
        &self,
        prefix: String,
        _: pb::AccessMode,
        _: Duration,
    ) -> impl Future<Output = Result<pb::ObjectAccess, Failure>> + Send {
        let s3 = pb::S3Access {
            endpoint: self.0.clone(),
            region: "us-east-1".into(),
            bucket: "backups".into(),
            key_prefix: prefix,
            access_key_id: USER.into(),
            secret_access_key: SECRET.into(),
            path_style: true,
            ..Default::default()
        };
        std::future::ready(Ok(pb::ObjectAccess {
            protocol: Some(Protocol::S3(Box::new(s3))),
            ..Default::default()
        }))
    }

    fn list(
        &self,
        _: String,
        _: String,
        _: u32,
    ) -> impl Future<Output = Result<pb::ObjectList, Failure>> + Send {
        std::future::ready(Ok(pb::ObjectList::default()))
    }

    fn delete(&self, _: Vec<String>) -> impl Future<Output = Result<u32, Failure>> + Send {
        std::future::ready(Ok(0))
    }
}

/// The volume the job backs up.
pub(crate) fn volume(at: &std::path::Path) {
    std::fs::create_dir_all(at.join("world/region")).unwrap();
    std::fs::write(at.join("server.properties"), b"motd=hello\n").unwrap();
    std::fs::write(
        at.join("world/level.dat"),
        (0..3_000_000u32).map(|i| u8::try_from(i % 251).unwrap()).collect::<Vec<u8>>(),
    )
    .unwrap();
}

/// The repository the job was granted.
fn repo_of(g: &korpis_proto::korpis::agent::v1::JobGrant) -> Repo<S3Store> {
    let s = g.store.as_option().unwrap();
    let config = S3Config {
        endpoint: s.endpoint.clone(),
        region: s.region.clone(),
        bucket: s.bucket.clone(),
        key_prefix: s.key_prefix.clone(),
        access_key_id: s.access_key_id.clone(),
        secret_access_key: s.secret_access_key.clone(),
        session_token: s.session_token.clone(),
        path_style: s.path_style,
        certificate: s.certificate.clone(),
    };
    let key: [u8; 32] = g.data_key.as_slice().try_into().unwrap();
    Repo::new(Arc::new(S3Store::new(config).unwrap()), Arc::new(Keys::derive(&key)))
}

/// The bytes of `path` in `backup`, read through the API as `config` may.
pub(crate) async fn read(
    h: &Harness,
    config: connectrpc::client::ClientConfig,
    backup: &str,
    path: &str,
) -> Result<Vec<u8>, connectrpc::ConnectError> {
    let r = pb::ReadBackupFileRequest {
        backup: backup.into(),
        path: path.into(),
        ..Default::default()
    };
    let mut stream = h.backups(config).read_backup_file(r).await?;
    let mut all = Vec::new();
    let mut size = None;
    while let Some(piece) = stream.message().await? {
        let piece = piece.to_owned_message();
        size.get_or_insert(piece.size);
        all.extend_from_slice(&piece.data);
    }
    assert_eq!(size, Some(u64::try_from(all.len()).unwrap()), "the length it said first");
    Ok(all)
}

fn names(entries: &[pb::BackupEntry]) -> Vec<String> {
    let mut n: Vec<String> =
        entries.iter().map(|e| String::from_utf8_lossy(&e.name).into_owned()).collect();
    n.sort();
    n
}

async fn browse(
    h: &Harness,
    config: connectrpc::client::ClientConfig,
    backup: &str,
    path: &str,
) -> Result<Vec<pb::BackupEntry>, connectrpc::ConnectError> {
    let r =
        pb::BrowseBackupRequest { backup: backup.into(), path: path.into(), ..Default::default() };
    Ok(h.backups(config).browse_backup(r).await?.into_owned().entries)
}

/// `backup.browse` browses and does not read; `backup.read` reads and does not browse.
async fn each_under_its_own_action(h: &Harness, id: &str) {
    let browser = holding(h, &["backup.browse", "workload.read"]).await;
    browse(h, browser.clone(), id, "world").await.unwrap();
    let e = read(h, browser, id, "world/level.dat").await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "browsing shows no byte");
    let reader = holding(h, &["backup.read", "workload.read"]).await;
    read(h, reader.clone(), id, "server.properties").await.unwrap();
    let e = browse(h, reader, id, "").await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "reading a file lists no other name");
}

#[tokio::test]
async fn a_backup_is_browsed_and_read_through_the_control_plane_each_under_its_own_action() {
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    minio.bucket("backups").await.unwrap();
    let (h, mut s, workload, vol, next) = running(vec![snapshotting()]).await;
    serving_with(&h, Bucket(minio.endpoint.clone())).await;
    let repo = repository(&h).await;
    let made =
        h.backups(h.root()).create_backup(create("b", &vol, &repo)).await.unwrap().into_owned();
    let id = made.backup.into_option().unwrap().id;
    let pick = assignment(|a| {
        a.plan.as_option().is_some_and(|p| p.steps.iter().any(|s| s.action == "backup_volume"))
    });
    let plan = until(&mut s, pick).await.plan.into_option().unwrap();
    let Outcome::Grant(g) = access(&mut s, "j", &plan.plan, plan.steps[0].index).await else {
        panic!("refused")
    };
    let source = scratch.path().join("volume");
    volume(&source);
    let options = Options {
        source: source.clone(),
        consistent: true,
        parent: None,
        content: Content::default(),
        concurrency: 4,
    };
    backup(&repo_of(&g), &bare(&id), options).await.unwrap();
    send(&mut s, vec![ended(next, &workload, &bare(&id), true)]).await;
    assert_eq!(until(&mut s, ack).await, next);
    complete(&mut s, &workload, &plan, next + 1).await;

    assert_eq!(
        names(&browse(&h, h.root(), &id, "").await.unwrap()),
        ["server.properties", "world"]
    );
    let world = browse(&h, h.root(), &id, "world").await.unwrap();
    assert_eq!(names(&world), ["level.dat", "region"]);
    let level = world.iter().find(|e| e.name == b"level.dat").unwrap();
    assert_eq!(
        (level.kind.as_known(), level.size),
        (Some(pb::BackupEntryKind::BACKUP_ENTRY_KIND_FILE), 3_000_000)
    );
    let bytes = read(&h, h.root(), &id, "world/level.dat").await.unwrap();
    assert_eq!(bytes, std::fs::read(source.join("world/level.dat")).unwrap());

    let e = browse(&h, h.root(), &id, "server.properties").await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument, "a file is not browsed");
    let e = read(&h, h.root(), &id, "../etc/passwd").await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    each_under_its_own_action(&h, &id).await;
    let store = h.db.store().await.unwrap();
    let recorded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM effect WHERE action IN ('backup.browse', 'backup.read')",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert!(recorded >= 7, "every read is recorded: {recorded}");
}
