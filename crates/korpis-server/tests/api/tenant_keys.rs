// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tenant key custody (§5.5 of docs/06-storage.md), against a real object store (MinIO; skipped
//! without it) and a key management provider served in the test: a repository naming the
//! tenant's key keeps no key-encryption key; a job's data key is made by the tenant's service,
//! wrapped for storage, and handed to the node sealed to the recipient its request named, the
//! same key each time; nothing the database holds is that key; the control plane reads the
//! backup by having the service seal the key to itself; and once the tenant revokes the key,
//! nobody, the control plane included, opens one.

use crate::backup_read::{read, volume};
use crate::backup_target::serving_with;
use crate::backups::{bare, create, ended};
use crate::extensions::{DNS, push_document};
use crate::journal::{ack, assignment, complete, send, until};
use crate::providers::installed_from;
use crate::snapshots::running;
use crate::support::{Harness, mutation, reason};
use korpis_backup::keys::Keys as BackupKeys;
use korpis_backup::s3::{S3Config, S3Store};
use korpis_backup::testing::Minio;
use korpis_backup::{Content, Options, Repo, backup};
use korpis_extension::Connection;
use korpis_extension::identity::{Credential, Fixed};
use korpis_extension::keys::{DataKey, Failure, KeyManagement, Keys};
use korpis_extension::keyseal::Recipient;
use korpis_extension::provider::{Provider, Registration};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use korpis_proto::korpis::v1 as pb;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// What the fake service holds: each wrapped key's data key and context, whether the tenant
/// revoked its key, and how many keys it opened.
#[derive(Default)]
struct Held {
    keys: HashMap<Vec<u8>, (DataKey, Vec<u8>)>,
    revoked: bool,
    opened: u32,
}

/// A key management service that wraps a key as a random handle, as an HSM does.
#[derive(Clone, Default)]
struct Service(Arc<Mutex<Held>>);

fn refused(reason: &str) -> Failure {
    Failure { reason: reason.into(), message: String::new(), retryable: false }
}

impl KeyManagement for Service {
    fn generate(
        &self,
        key: String,
        context: Vec<u8>,
    ) -> impl Future<Output = Result<(DataKey, Vec<u8>), Failure>> + Send {
        let mut held = self.0.lock().unwrap();
        let made = (|| {
            if held.revoked || key != "tenant-key-1" {
                return Err(refused("kms.denied"));
            }
            let data: DataKey = uuid::Uuid::now_v7().as_bytes().repeat(2).try_into().unwrap();
            let handle = uuid::Uuid::now_v7().as_bytes().to_vec();
            held.keys.insert(handle.clone(), (data, context));
            Ok((data, handle))
        })();
        std::future::ready(made)
    }

    fn unwrap(
        &self,
        _: String,
        context: Vec<u8>,
        wrapped: Vec<u8>,
    ) -> impl Future<Output = Result<DataKey, Failure>> + Send {
        let mut held = self.0.lock().unwrap();
        let opened = match held.keys.get(&wrapped).cloned() {
            _ if held.revoked => Err(refused("kms.denied")),
            Some((data, bound)) if bound == context => Ok(data),
            _ => Err(refused("kms.context")),
        };
        held.opened += u32::from(opened.is_ok());
        std::future::ready(opened)
    }
}

/// The tenant's service, as the root organization's key management provider `vault`.
async fn kms_serving(h: &Harness, service: Service) {
    let document = DNS
        .replace("name: cloudflare-dns", "name: tenant-vault")
        .replace("    - {type: dns, name: cloudflare}", "    - {type: key_management, name: vault}")
        .replace(
            "requires: [provider.serve, provider.dns,",
            "requires: [provider.serve, provider.key_management,",
        );
    let x = installed_from(h, 73, "keys", push_document(&document).await).await;
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
    })));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_KEY_MANAGEMENT,
        name: "vault".into(),
        version: "1.0.0".into(),
        concurrency: 4,
    };
    let provider = Arc::new(Provider::new(connection, registration, Keys(service)));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
}

fn repository(
    key: &str,
    custody: pb::KeyCustody,
    kms: Option<&str>,
) -> pb::CreateRepositoryRequest {
    pb::CreateRepositoryRequest {
        mutation: mutation(key).into(),
        name: key.into(),
        provider: "local".into(),
        prefix: format!("{key}/"),
        key_custody: custody.into(),
        kms: kms
            .map(|k| pb::TenantKey {
                provider: "vault".into(),
                key: k.into(),
                ..Default::default()
            })
            .into(),
        retention: pb::RetentionPolicy { keep_daily: 7, ..Default::default() }.into(),
        ..Default::default()
    }
}

/// Asks for a job's access naming `recipient`; the outcome.
async fn access(
    s: &mut crate::agent::Stream,
    request: &str,
    (plan, step): (&str, u32),
    recipient: &[u8],
) -> Outcome {
    let up = agent::JobAccessRequest {
        request: request.into(),
        plan: plan.into(),
        step,
        recipient: recipient.to_vec(),
        ..Default::default()
    };
    s.send(agent::AgentMessage {
        message: Some(Up::JobAccessRequest(Box::new(up))),
        ..Default::default()
    })
    .await
    .unwrap();
    let pick = |d: &Down| match d {
        Down::JobAccess(a) if a.request == request => a.outcome.clone(),
        _ => None,
    };
    until(s, pick).await
}

fn opened(g: &agent::JobGrant, recipient: &Recipient) -> [u8; 32] {
    assert!(g.data_key.is_empty(), "never in the clear");
    let context = format!("{}/{}", g.repository, g.workload);
    recipient.open(&g.sealed_data_key, context.as_bytes()).unwrap()
}

fn repo_of(g: &agent::JobGrant, key: &[u8; 32]) -> Repo<S3Store> {
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
    };
    Repo::new(Arc::new(S3Store::new(config).unwrap()), Arc::new(BackupKeys::derive(key)))
}

/// The repository under tenant custody, once the two ways of asking for one wrongly are refused.
async fn tenant_repository(h: &Harness) -> pb::Repository {
    let repos = h.repositories(h.root());
    let mut bad = repository("op", pb::KeyCustody::KEY_CUSTODY_OPERATOR, Some("tenant-key-1"));
    bad.organization = h.root_org();
    assert_eq!(reason(&repos.create_repository(bad).await.unwrap_err()), "request.invalid");
    let mut bad = repository("none", pb::KeyCustody::KEY_CUSTODY_TENANT, None);
    bad.organization = h.root_org();
    assert_eq!(reason(&repos.create_repository(bad).await.unwrap_err()), "request.invalid");
    let mut made = repository("tenant", pb::KeyCustody::KEY_CUSTODY_TENANT, Some("tenant-key-1"));
    made.organization = h.root_org();
    let repo = repos.create_repository(made).await.unwrap().into_owned().repository.into_option();
    let repo = repo.unwrap();
    assert_eq!(repo.kms.as_option().map(|k| k.key.as_str()), Some("tenant-key-1"));
    repo
}

/// That no call the control plane made, and no key it keeps, holds `key`.
async fn nothing_stored_is(h: &Harness, key: &[u8; 32]) {
    let store = h.db.store().await.unwrap();
    let hex = key.iter().fold(String::new(), |mut s, b| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    });
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT coalesce(request::text, '') || coalesce(outcome::text, '') FROM provider_call",
    )
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert!(rows.iter().all(|r| !r.contains(&hex)), "no call holds the key");
    let kept: Vec<u8> = sqlx::query_scalar("SELECT dek_sealed FROM workload_key")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert!(
        !kept.windows(32).any(|w| w == key.as_slice()),
        "what is kept is the service's wrapping"
    );
}

#[tokio::test]
async fn a_tenant_s_key_is_sealed_to_whoever_uses_it_and_revoking_it_ends_every_use() {
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = Minio::start(scratch.path()).await.unwrap() else { return };
    minio.bucket("backups").await.unwrap();
    let (h, mut s, workload, vol, next) = running(Vec::new()).await;
    let service = Service::default();
    kms_serving(&h, service.clone()).await;
    serving_with(&h, crate::prunes::real(&minio)).await;
    let repo = tenant_repository(&h).await;

    // The job's key: made by the tenant's service, sealed to the job, the same each time.
    let made = h.backups(h.root()).create_backup(create("b", &vol, &repo.id)).await.unwrap();
    let id = made.into_owned().backup.into_option().unwrap().id;
    let pick = assignment(|a| {
        a.plan.as_option().is_some_and(|p| p.steps.iter().any(|s| s.action == "backup_volume"))
    });
    let plan = until(&mut s, pick).await.plan.into_option().unwrap();
    let step = (plan.plan.as_str(), plan.steps[0].index);
    let Outcome::Refused(e) = access(&mut s, "bare", step, &[]).await else { panic!() };
    assert!(!e.retryable, "a job that names no recipient is never handed a tenant's key");
    let job = Recipient::new().unwrap();
    let Outcome::Grant(g) = access(&mut s, "j1", step, job.public()).await else { panic!() };
    let key = opened(&g, &job);
    let again = Recipient::new().unwrap();
    let Outcome::Grant(g2) = access(&mut s, "j2", step, again.public()).await else { panic!() };
    assert_eq!(opened(&g2, &again), key, "one key per workload, opened by the service each time");

    let source = scratch.path().join("volume");
    volume(&source);
    let options = Options {
        source: source.clone(),
        consistent: true,
        parent: None,
        content: Content::default(),
        concurrency: 4,
    };
    backup(&repo_of(&g, &key), &bare(&id), options).await.unwrap();
    send(&mut s, vec![ended(next, &workload, &bare(&id), true)]).await;
    assert_eq!(until(&mut s, ack).await, next);
    complete(&mut s, &workload, &plan, next + 1).await;

    // The control plane reads it only by having the service seal the key to itself.
    let before = service.0.lock().unwrap().opened;
    let bytes = read(&h, h.root(), &id, "server.properties").await.unwrap();
    assert_eq!(bytes, std::fs::read(source.join("server.properties")).unwrap());
    assert!(service.0.lock().unwrap().opened > before, "an unwrap the tenant's service saw");

    nothing_stored_is(&h, &key).await;

    // Revoked: the job is refused, to be asked again, and the control plane reads nothing.
    service.0.lock().unwrap().revoked = true;
    h.backups(h.root()).create_backup(create("b2", &vol, &repo.id)).await.unwrap();
    let pick = assignment(|a| {
        a.plan.as_option().is_some_and(|p| p.plan != plan.plan && !p.steps.is_empty())
    });
    let later = until(&mut s, pick).await.plan.into_option().unwrap();
    let step = (later.plan.as_str(), later.steps[0].index);
    let late = Recipient::new().unwrap();
    let Outcome::Refused(e) = access(&mut s, "j3", step, late.public()).await else { panic!() };
    assert!(e.retryable, "{e:?}");
    let e = read(&h, h.root(), &id, "server.properties").await.unwrap_err();
    assert_eq!(e.code, connectrpc::ErrorCode::Unavailable);
}
