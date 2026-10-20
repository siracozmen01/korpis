// SPDX-License-Identifier: AGPL-3.0-or-later

//! Calling a backup target provider (§5.6 of docs/06-storage.md): access scoped to a prefix
//! whose secret the record of the call keeps only until it is read, and the listing and deletion
//! a prune runs with, each answer judged before anything acts on it.

use crate::extensions::{DNS, push_document};
use crate::providers::installed_from;
use crate::support::Harness;
use korpis_app::extension::{CallTarget, Called};
use korpis_core::extension::{AccessMode, CallOutcome, ProviderType};
use korpis_extension::backup::{Backup, BackupTarget, Failure};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::object_access::Protocol;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// A bucket in memory. Under `evil/` it misbehaves: its access and its listings stray outside
/// the prefix asked for.
#[derive(Clone, Default)]
pub struct Bucket(Arc<Mutex<BTreeSet<String>>>);

impl Bucket {
    fn keys(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl BackupTarget for Bucket {
    fn grant_access(
        &self,
        prefix: String,
        _mode: pb::AccessMode,
        _ttl: Duration,
    ) -> impl Future<Output = Result<pb::ObjectAccess, Failure>> + Send {
        let key_prefix =
            if prefix.starts_with("evil/") { "other/".into() } else { format!("repo/{prefix}") };
        let s3 = pb::S3Access {
            endpoint: "https://s3.example".into(),
            bucket: "backups".into(),
            key_prefix,
            access_key_id: "AKIA".into(),
            secret_access_key: "very-secret".into(),
            session_token: "token".into(),
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
        prefix: String,
        after: String,
        limit: u32,
    ) -> impl Future<Output = Result<pb::ObjectList, Failure>> + Send {
        let mut keys: Vec<String> =
            self.keys().iter().filter(|k| k.starts_with(&prefix) && **k > after).cloned().collect();
        if prefix.starts_with("evil/") {
            keys.push("workloads/someone-else/x".into());
        }
        let more = keys.len() > usize::try_from(limit).unwrap_or(0);
        keys.truncate(usize::try_from(limit).unwrap_or(0));
        let objects = keys
            .into_iter()
            .map(|key| pb::StoredObject { key, size: 1, ..Default::default() })
            .collect();
        std::future::ready(Ok(pb::ObjectList { objects, more, ..Default::default() }))
    }

    fn delete(&self, keys: Vec<String>) -> impl Future<Output = Result<u32, Failure>> + Send {
        let mut held = self.keys();
        let n = keys.iter().filter(|k| held.remove(*k)).count();
        std::future::ready(Ok(u32::try_from(n).unwrap_or(u32::MAX)))
    }
}

/// A backup target named `local` serving the root organization from `bucket`.
pub async fn serving(h: &Harness, bucket: Bucket) -> CallTarget {
    serving_with(h, bucket).await
}

/// A backup target named `local` serving the root organization as `target` answers.
pub async fn serving_with<T: BackupTarget + Send + Sync + 'static>(
    h: &Harness,
    target: T,
) -> CallTarget {
    let document = DNS
        .replace("    - {type: dns, name: cloudflare}", "    - {type: backup_target, name: local}")
        .replace(
            "requires: [provider.serve, provider.dns,",
            "requires: [provider.serve, provider.backup_target,",
        );
    let x = installed_from(h, 71, "backups", push_document(&document).await).await;
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_BACKUP_TARGET,
        name: "local".into(),
        version: "1.0.0".into(),
        concurrency: 4,
        sign_in: false,
    };
    let provider = Arc::new(Provider::new(connection, registration, Backup(target)));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
    let org = korpis_core::id::Id::parse(korpis_core::id::IdKind::Organization, &h.root_org());
    CallTarget {
        organization: org.unwrap().uuid(),
        kind: ProviderType::BackupTarget,
        name: "local".into(),
    }
}

/// The stored answer of a call, as the database holds it.
async fn stored(h: &Harness, called: &Called) -> serde_json::Value {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar("SELECT outcome FROM provider_call WHERE id = $1")
        .bind(called.id)
        .fetch_one(store.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn access_is_scoped_and_its_secret_does_not_stay_in_the_database() {
    let h = Harness::new().await;
    let bucket = Bucket(Arc::default());
    let target = serving(&h, bucket.clone()).await;
    let app = h.api.app();

    let ttl = Duration::from_secs(3600);
    let granted =
        app.grant_backup_access(&target, "workloads/wl_1/", AccessMode::Write, ttl).await.unwrap();
    let CallOutcome::Access(access) = &granted.outcome else { panic!("{:?}", granted.outcome) };
    assert_eq!(
        (access.s3.key_prefix.as_str(), access.s3.secret_access_key.as_str()),
        ("repo/workloads/wl_1/", "very-secret")
    );
    assert!(access.s3.path_style, "the provider says how its store is addressed");
    let kept = stored(&h, &granted).await;
    assert!(kept["access"]["s3"].get("secret_access_key").is_none(), "redacted once read: {kept}");
    assert!(kept["access"]["s3"].get("session_token").is_none());

    let stray =
        app.grant_backup_access(&target, "evil/wl_2/", AccessMode::Write, ttl).await.unwrap();
    let CallOutcome::Failed(f) = stray.outcome else { panic!("refused") };
    assert_eq!(f.reason, "provider.malformed", "access beyond the prefix never reaches a node");
    let short = app
        .grant_backup_access(&target, "workloads/wl_1/", AccessMode::Read, Duration::from_secs(5))
        .await;
    assert!(matches!(short, Err(korpis_app::AppError::Invalid { field, .. }) if field == "ttl"));
}

#[tokio::test]
async fn a_prune_lists_and_deletes_only_beneath_its_prefix() {
    let h = Harness::new().await;
    let bucket = Bucket(Arc::default());
    bucket.keys().extend([
        "workloads/wl_1/a".into(),
        "workloads/wl_1/b".into(),
        "workloads/wl_2/c".into(),
    ]);
    let target = serving(&h, bucket.clone()).await;
    let app = h.api.app();

    let listed = app.list_backup_objects(&target, "workloads/wl_1/", "", 1).await.unwrap();
    let CallOutcome::Objects { objects, more } = listed.outcome else { panic!("a listing") };
    assert_eq!((objects.len(), more), (1, true));
    let stray = app.list_backup_objects(&target, "evil/", "", 10).await.unwrap();
    assert!(matches!(stray.outcome, CallOutcome::Failed(f) if f.reason == "provider.malformed"));

    let keys = vec!["workloads/wl_1/a".to_owned(), "workloads/wl_1/b".to_owned()];
    let deleted = app.delete_backup_objects(&target, "workloads/wl_1/", keys).await.unwrap();
    assert_eq!(deleted.outcome, CallOutcome::Deleted { count: 2 });
    assert_eq!(bucket.keys().iter().collect::<Vec<_>>(), ["workloads/wl_2/c"]);
    let outside = vec!["workloads/wl_2/c".to_owned()];
    let refused = app.delete_backup_objects(&target, "workloads/wl_1/", outside).await;
    assert!(
        matches!(refused, Err(korpis_app::AppError::Invalid { field, .. }) if field == "keys[0]")
    );
}
