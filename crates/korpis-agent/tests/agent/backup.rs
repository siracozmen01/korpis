// SPDX-License-Identifier: AGPL-3.0-or-later

//! A backup through the agent against a real control plane (§5 of docs/06-storage.md), with the
//! in-memory runtime and helper: the backup target is an extension installed on the same node,
//! serving through the SDK with the credential its identity socket gives it; the agent asks for
//! the job's key and access when the step begins, starts the job by the backup's name, reports
//! how it ended; the backup is whole once the control plane believes that; a backup restores
//! into a directory at once and over the volume once approved; and every job is forgotten once
//! its plan has left the set.

use crate::session::until;
use crate::support::{ControlPlane, facts, mutation};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_extension::backup::{Backup, BackupTarget, Failure};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, IdentitySocket};
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::object_access::Protocol;
use korpis_proto::korpis::v1::__buffa::oneof::restore_backup_request::Target;
use korpis_recipe::artifact::Artifact;
use korpis_recipe::registry::Registry;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// A backup target that grants access to whatever prefix it is asked for, and holds nothing.
struct Granting;

impl BackupTarget for Granting {
    fn grant_access(
        &self,
        prefix: String,
        _mode: pb::AccessMode,
        _ttl: Duration,
    ) -> impl Future<Output = Result<pb::ObjectAccess, Failure>> + Send {
        let s3 = pb::S3Access {
            endpoint: "https://s3.example".into(),
            bucket: "backups".into(),
            key_prefix: prefix,
            access_key_id: "AKIA".into(),
            secret_access_key: "secret".into(),
            ..Default::default()
        };
        let access =
            pb::ObjectAccess { protocol: Some(Protocol::S3(Box::new(s3))), ..Default::default() };
        std::future::ready(Ok(access))
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

/// The DNS extension of the recipe tests, made a backup target named `local`, pushed unsigned
/// (the agent's tests run unsigned recipes); its repository.
async fn push_target() -> String {
    let document = include_str!("../../../korpis-recipe/tests/recipe/dns.yaml")
        .replace("    - {type: dns, name: cloudflare}", "    - {type: backup_target, name: local}")
        .replace(
            "requires: [provider.serve, provider.dns,",
            "requires: [provider.serve, provider.backup_target,",
        );
    let registry =
        std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned());
    let repo = format!("{registry}/test/target-{}", uuid::Uuid::now_v7().simple());
    let artifact = Artifact { document: document.into_bytes(), files: Vec::new() };
    Registry::new(vec![registry]).unwrap().push(&repo, "1.0.0", &artifact).await.unwrap();
    repo
}

/// Installs the target in `project` and serves it from its socket under `root`, once running.
async fn serve_target(cp: &ControlPlane, project: &str, root: &Path) {
    let artifact = push_target().await;
    let extensions = cp.client(pb::ExtensionServiceClient::new);
    let install = |key: &str| pb::InstallExtensionRequest {
        mutation: mutation(key).into(),
        artifact: artifact.clone(),
        version: "1.0.0".into(),
        organization: cp.root_org.clone(),
        workload_project: project.to_owned(),
        ..Default::default()
    };
    let dry = pb::InstallExtensionRequest { dry_run: true, ..install("dry") };
    let shown = extensions.install_extension(dry).await.unwrap().into_owned();
    let needs = shown.extension.into_option().unwrap().manifest.into_option().unwrap().needs;
    let done = pb::InstallExtensionRequest { approved_needs: needs, ..install("install") };
    let installed = extensions.install_extension(done).await.unwrap().into_owned();
    let workload = installed.extension.into_option().unwrap().workload;
    let socket = root.join(&workload).join("identity.sock");
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let connection = Arc::new(Connection::new(IdentitySocket::at(&socket)));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_BACKUP_TARGET,
        name: "local".into(),
        version: "1.0.0".into(),
        concurrency: 4,
    };
    let provider = Arc::new(Provider::new(connection, registration, Backup(Granting)));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
}

async fn start(
    cp: &ControlPlane,
    dir: &Path,
    runtime: &InMemory,
    host: &InMemoryHost,
) -> (Agent, PathBuf) {
    let (token, ca) = cp.token("fra-4").await;
    let state = StateDir::new(dir.join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let root = dir.join("identity");
    let options = Options {
        period: Duration::from_millis(200),
        identity_root: Some(root.clone()),
        ..Options::default()
    };
    let agent =
        Agent::start(&state, runtime.clone(), host.clone(), Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    (agent, root)
}

async fn backup(cp: &ControlPlane, id: &str) -> pb::Backup {
    let r = pb::GetBackupRequest { id: id.into(), ..Default::default() };
    let got = cp.client(pb::BackupServiceClient::new).get_backup(r).await.unwrap().into_owned();
    got.backup.into_option().unwrap()
}

/// A repository `nightly` at the target, and a backup of `project`'s volume `data` into it:
/// the backup and its plan.
async fn backed_up(cp: &ControlPlane, project: &str) -> (String, String) {
    let volume = cp.volume_named(project, "data").await;
    let repository = pb::CreateRepositoryRequest {
        mutation: mutation("repo").into(),
        organization: cp.root_org.clone(),
        name: "nightly".into(),
        provider: "local".into(),
        prefix: "korpis/".into(),
        retention: pb::RetentionPolicy { keep_daily: 7, ..Default::default() }.into(),
        ..Default::default()
    };
    let repos = cp.client(pb::RepositoryServiceClient::new);
    let repo = repos
        .create_repository(repository)
        .await
        .unwrap()
        .into_owned()
        .repository
        .into_option()
        .unwrap()
        .id;
    let create = pb::CreateBackupRequest {
        mutation: mutation("b").into(),
        volume,
        repository: repo,
        ..Default::default()
    };
    let made =
        cp.client(pb::BackupServiceClient::new).create_backup(create).await.unwrap().into_owned();
    let id = made.backup.into_option().unwrap().id;

    (id, made.plan.into_option().unwrap().id)
}

/// Waits for `plan` to be applied.
async fn applied(cp: &ControlPlane, plan: &str) {
    let mut status = cp.plan(plan).await.status.as_known();
    for _ in 0..150 {
        if status == Some(pb::PlanStatus::PLAN_STATUS_APPLIED) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        status = cp.plan(plan).await.status.as_known();
    }
    assert_eq!(status, Some(pb::PlanStatus::PLAN_STATUS_APPLIED), "{plan}");
}

/// Waits for the helper to keep no job: each forgotten once its plan left the set.
async fn no_jobs(host: &InMemoryHost) {
    for _ in 0..200 {
        if korpis_agent::host::Host::list_jobs(host).await.unwrap().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("jobs kept after their plans: {:?}", korpis_agent::host::Host::list_jobs(host).await);
}

/// Restores backup `id` as `target` asks; the plan.
async fn restored(cp: &ControlPlane, key: &str, id: &str, target: Target) -> pb::Plan {
    let r = pb::RestoreBackupRequest {
        mutation: mutation(key).into(),
        backup: id.into(),
        target: Some(target),
        ..Default::default()
    };
    let made = cp.client(pb::BackupServiceClient::new).restore_backup(r).await.unwrap();
    made.into_owned().plan.into_option().unwrap()
}

/// The backup, once it is no longer pending.
async fn settled(cp: &ControlPlane, id: &str) -> pb::Backup {
    let mut taken = backup(cp, id).await;
    for _ in 0..200 {
        if taken.status.as_known() != Some(pb::BackupStatus::BACKUP_STATUS_PENDING) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        taken = backup(cp, id).await;
    }
    taken
}

#[tokio::test]
async fn the_agent_takes_a_backup_with_the_access_it_is_granted_and_reports_it() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let (agent, root) = start(&cp, dir.path(), &runtime, &host).await;
    serve_target(&cp, &project, &root).await;
    // The jar the recipe's install fetches, as the node's content store holds it.
    let jar = format!("sha256:{}", "1".repeat(64));
    host.offer_content(&jar, b"a jar");
    cp.declare(&project, "mc").await;
    let (id, plan) = backed_up(&cp, &project).await;
    let taken = settled(&cp, &id).await;
    let job = host.started_jobs().into_iter().find(|j| j.starts_with("bkp")).expect("a backup job");
    let Some(Work::Backup(work)) = host.started(&job).unwrap().work else { panic!("a backup") };
    let content: Vec<_> = work.content.iter().map(|c| (c.size, c.sha256.clone())).collect();
    assert_eq!(content, [(5, vec![0x11; 32])], "the recipe's content, by reference");
    assert_eq!(
        taken.status.as_known(),
        Some(pb::BackupStatus::BACKUP_STATUS_SUCCEEDED),
        "{taken:?}"
    );
    assert!(!taken.consistent, "a class that cannot snapshot is read live, and says so");
    assert_eq!(taken.files, 1);
    applied(&cp, &plan).await;
    no_jobs(&host).await;
    drop(agent);
}

#[tokio::test]
async fn the_agent_restores_into_a_directory_and_over_the_volume_once_approved() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let (agent, root) = start(&cp, dir.path(), &runtime, &host).await;
    serve_target(&cp, &project, &root).await;
    cp.declare(&project, "mc").await;
    let (id, plan) = backed_up(&cp, &project).await;
    settled(&cp, &id).await;
    applied(&cp, &plan).await;

    let into = restored(&cp, "into", &id, Target::Into("restored".into())).await;
    applied(&cp, &into.id).await;
    let replace = restored(&cp, "replace", &id, Target::Replace(true)).await;
    let approve = pb::ApprovePlanRequest {
        mutation: mutation("approve").into(),
        plan: replace.id.clone(),
        ..Default::default()
    };
    cp.client(pb::PlanServiceClient::new).approve_plan(approve).await.unwrap();
    applied(&cp, &replace.id).await;
    no_jobs(&host).await;
    drop(agent);
}

#[tokio::test]
async fn a_new_workload_is_created_holding_a_backup_before_it_first_runs() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let (agent, root) = start(&cp, dir.path(), &runtime, &host).await;
    serve_target(&cp, &project, &root).await;
    cp.declare(&project, "mc").await;
    let (id, plan) = backed_up(&cp, &project).await;
    settled(&cp, &id).await;
    applied(&cp, &plan).await;

    let seeded =
        pb::VolumeSpec { name: "data".into(), from_backup: Some(id), ..Default::default() };
    let copy = cp.declare_with(&project, "mc-copy", vec![seeded]).await;
    // Three minutes: the restore runs a real job, which takes seconds alone and has taken over a
    // minute while every other test runs beside it.
    for _ in 0..1800 {
        let s = cp.status(&copy).await;
        if s.convergence.as_known() == Some(pb::Convergence::CONVERGENCE_CONVERGED) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        cp.status(&copy).await.convergence.as_known(),
        Some(pb::Convergence::CONVERGENCE_CONVERGED),
        "restored, then started"
    );
    no_jobs(&host).await;
    drop(agent);
}
