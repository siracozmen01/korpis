// SPDX-License-Identifier: AGPL-3.0-or-later

//! A container's life in containerd. `create` imports and unpacks the image under a lease,
//! prepares the container's own snapshot remapped onto the workload's id range, and records the
//! container with its runtime spec and the labels adoption matches it by; nothing runs. `start`
//! runs a task on it, replacing one that exited; `stop` signals, waits out the grace, and kills;
//! `destroy` releases everything the container holds. Volumes are never touched here.

use super::{Containerd, NAMESPACE, RUNTIME, SNAPSHOTTER, under};
use crate::error::{DriverError, DriverResult};
use crate::oci::runtime_spec;
use crate::validate::{Policy, Valid, Violation};
use buffa_types::google::protobuf::Any;
use korpis_containerd::containerd::services::containers::v1 as containers;
use korpis_containerd::containerd::services::snapshots::v1 as snapshots;
use korpis_containerd::containerd::services::tasks::v1 as tasks;
use korpis_containerd::containerd::v1::types::Status;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// The type containerd stores a runtime spec under.
const SPEC_TYPE: &str = "types.containerd.io/opencontainers/runtime-spec/1/Spec";
/// The labels adoption matches a container by (§4.4 of docs/02-architecture.md).
pub(crate) const LABEL_WORKLOAD: &str = "korpis.workload";
pub(crate) const LABEL_INTENT: &str = "korpis.intent";
const LABEL_GENERATION: &str = "korpis.assignment_generation";
const LABEL_CYCLE: &str = "korpis.power_cycle";
const LABEL_EPOCH: &str = "korpis.lease_epoch";
pub(crate) const LABEL_RUN: &str = "korpis.run";
/// How long a killed task is given to be reaped.
const KILL_WAIT: Duration = Duration::from_secs(10);

/// The container identifier of `workload` built from `intent`, for `run` when it executes one:
/// one per intent and run, so a create is idempotent, a rebuild never collides with the object
/// it replaces, and two runs never share one. A digest of them, so it is always within
/// containerd's 76 characters and its alphabet, whatever it is given; the labels carry them
/// readably. A persistent workload's object, which executes no run, keeps the digest it had
/// before runs, so an upgraded driver finds it again.
pub(crate) fn container_id(workload: &str, intent: &str, run: &str) -> String {
    let named = if run.is_empty() {
        format!("{workload}\n{intent}")
    } else {
        format!("{workload}\n{intent}\n{run}")
    };
    let digest = korpis_core::content::digest_of(named.as_bytes());
    let hex = digest.trim_start_matches("sha256:");
    format!("korpis-{}", hex.get(..40).unwrap_or(hex))
}

/// A driver-declared signal name, as a number.
pub fn signal_number(name: &str) -> DriverResult<u32> {
    Ok(match name {
        "" | "SIGTERM" => 15,
        "SIGKILL" => 9,
        "SIGINT" => 2,
        "SIGHUP" => 1,
        "SIGQUIT" => 3,
        "SIGUSR1" => 10,
        "SIGUSR2" => 12,
        "SIGWINCH" => 28,
        other => return Err(DriverError::Rejected(format!("no signal named {other:?}"))),
    })
}

/// Every volume source, resolved on the filesystem, is a directory under the volume root: the
/// lexical check of [`crate::validate`] cannot see a symbolic link.
fn volumes_resolve(valid: &Valid, policy: &Policy) -> DriverResult<()> {
    let root = std::fs::canonicalize(&policy.volume_root)?;
    for m in &valid.spec().mounts {
        let real = std::fs::canonicalize(&m.source).map_err(|e| {
            DriverError::Rejected(format!("volume {:?} is not prepared: {e}", m.volume))
        })?;
        if !real.starts_with(&root) || real == root || !real.is_dir() {
            let why = "a volume resolves to a directory under the volume root";
            return Err(Violation { field: "mounts.source".into(), why: why.into() }.into());
        }
    }
    identity_resolves(valid, policy)
}

/// The identity directory, if any, resolves to the workload's own directory under the identity
/// root, and not through a symbolic link to another workload's.
fn identity_resolves(valid: &Valid, policy: &Policy) -> DriverResult<()> {
    let spec = valid.spec();
    if spec.identity_dir.is_empty() {
        return Ok(());
    }
    let own = std::fs::canonicalize(&policy.identity_root)?.join(&spec.workload);
    let real = std::fs::canonicalize(&spec.identity_dir)
        .map_err(|e| DriverError::Rejected(format!("the identity directory is not made: {e}")))?;
    if real != own || !real.is_dir() {
        let why = "the identity directory resolves to the workload's own";
        return Err(Violation { field: "identity_dir".into(), why: why.into() }.into());
    }
    Ok(())
}

fn labels(valid: &Valid) -> BTreeMap<String, String> {
    let l = valid.spec().labels.as_option().cloned().unwrap_or_default();
    BTreeMap::from([
        (LABEL_WORKLOAD.to_owned(), l.workload),
        (LABEL_INTENT.to_owned(), l.intent),
        (LABEL_GENERATION.to_owned(), l.assignment_generation.to_string()),
        (LABEL_CYCLE.to_owned(), l.power_cycle.to_string()),
        (LABEL_EPOCH.to_owned(), l.lease_epoch.to_string()),
        (LABEL_RUN.to_owned(), l.run),
    ])
}

impl Containerd {
    fn containers(&self) -> containers::ContainersClient<super::Transport> {
        self.containers_client()
    }

    pub(crate) fn containers_client(&self) -> containers::ContainersClient<super::Transport> {
        containers::ContainersClient::new(self.conn.clone(), self.config.clone())
    }

    pub(crate) fn tasks(&self) -> tasks::TasksClient<super::Transport> {
        tasks::TasksClient::new(self.conn.clone(), self.config.clone())
    }

    /// Where a task of container `id` writes its stdout and stderr: the stream writer, if the
    /// container has a stream, and nowhere otherwise; and, for a console that takes input, the
    /// stdin the writer feeds, read through a terminal.
    async fn stdio(&self, id: &str) -> DriverResult<(String, Option<String>)> {
        let Some(streams) = &self.streams else { return Ok((String::new(), None)) };
        let request = containers::GetContainerRequest { id: id.to_owned(), ..Default::default() };
        let container = self.containers().get(request).await?.into_owned().container;
        let labels = container.as_option().map(|c| c.labels.clone()).unwrap_or_default();
        let labels: BTreeMap<String, String> = labels.into_iter().collect();
        Ok((streams.stdio(id, &labels).unwrap_or_default(), streams.stdin(id, &labels)))
    }

    /// Whether the container `id` exists.
    pub(crate) async fn container_exists(&self, id: &str) -> DriverResult<bool> {
        let request = containers::GetContainerRequest { id: id.to_owned(), ..Default::default() };
        match self.containers().get(request).await {
            Ok(_) => Ok(true),
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Creates the container `valid` describes; its identifier. Nothing runs yet.
    pub async fn create(&self, valid: &Valid, policy: &Policy) -> DriverResult<String> {
        let s = valid.spec();
        let run = s.labels.as_option().map(|l| l.run.as_str()).unwrap_or_default();
        let id = container_id(&s.workload, &s.intent, run);
        if self.container_exists(&id).await? {
            return Ok(id);
        }
        volumes_resolve(valid, policy)?;
        if valid.spec().stream.is_set() {
            let streams = self.streams.as_ref().ok_or_else(|| {
                DriverError::Rejected("this driver has no stream writer configured".into())
            })?;
            streams.write_secrets(valid, &id)?;
            streams.make_input(valid, &id)?;
        }
        let lease = self.lease().await?;
        let made = self.assemble(&lease, valid, &id).await;
        self.release(&lease).await;
        made.map(|()| id)
    }

    /// Imports, unpacks, snapshots, and records the container, all under `lease`.
    async fn assemble(&self, lease: &str, valid: &Valid, id: &str) -> DriverResult<()> {
        let s = valid.spec();
        let image = self.import(lease, &s.rootfs).await?;
        let top = self.unpack(lease, &image).await?;
        let map = format!("0:{}:{}", s.uid_base, s.uid_count);
        let prepare = snapshots::PrepareSnapshotRequest {
            snapshotter: SNAPSHOTTER.into(),
            key: id.to_owned(),
            parent: top,
            labels: [
                ("containerd.io/snapshot/uidmapping".to_owned(), map.clone()),
                ("containerd.io/snapshot/gidmapping".to_owned(), map),
            ]
            .into(),
            ..Default::default()
        };
        let client = snapshots::SnapshotsClient::new(self.conn.clone(), self.config.clone());
        match client.prepare_with_options(prepare, under(lease)).await {
            Ok(_) => {}
            Err(e) if e.code == connectrpc::ErrorCode::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let spec = runtime_spec(valid, &image.config, &self.cgroup(id))
            .map_err(|e| DriverError::Rejected(e.to_string()))?;
        let json = serde_json::to_vec(&spec).map_err(|e| DriverError::Failed(e.to_string()))?;
        let mut labels = labels(valid);
        labels.extend(super::stream::labels(valid));
        let container = containers::Container {
            id: id.to_owned(),
            labels,
            image: s.rootfs.clone(),
            runtime: containers::container::Runtime { name: RUNTIME.into(), ..Default::default() }
                .into(),
            spec: Any { type_url: SPEC_TYPE.into(), value: json.into(), ..Default::default() }
                .into(),
            snapshotter: SNAPSHOTTER.into(),
            snapshot_key: id.to_owned(),
            ..Default::default()
        };
        let request = containers::CreateContainerRequest {
            container: container.into(),
            ..Default::default()
        };
        self.containers().create_with_options(request, under(lease)).await?;
        tracing::info!(%id, namespace = NAMESPACE, "container created");
        Ok(())
    }

    /// The status of the task on `id`, if there is one.
    pub(crate) async fn task(&self, id: &str) -> DriverResult<Option<(Status, u32, u32)>> {
        let request = tasks::GetRequest { container_id: id.to_owned(), ..Default::default() };
        match self.tasks().get(request).await {
            Ok(r) => {
                let r = r.into_owned();
                let p = r.process.as_option().cloned().unwrap_or_default();
                let status = p.status.as_known().unwrap_or(Status::UNKNOWN);
                Ok(Some((status, p.exit_status, p.pid)))
            }
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn delete_task(&self, id: &str) -> DriverResult<()> {
        let request =
            tasks::DeleteTaskRequest { container_id: id.to_owned(), ..Default::default() };
        match self.tasks().delete(request).await {
            Ok(_) => Ok(()),
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Runs the container `id`, replacing a task that exited.
    pub async fn start(&self, id: &str) -> DriverResult<()> {
        match self.task(id).await? {
            Some((Status::RUNNING, _, _)) => return Ok(()),
            Some(_) => self.delete_task(id).await?,
            None => {}
        }
        if !self.container_exists(id).await? {
            return Err(DriverError::NotFound(id.to_owned()));
        }
        let mounts = snapshots::MountsRequest {
            snapshotter: SNAPSHOTTER.into(),
            key: id.to_owned(),
            ..Default::default()
        };
        let client = snapshots::SnapshotsClient::new(self.conn.clone(), self.config.clone());
        let rootfs = client.mounts(mounts).await?.into_owned().mounts;
        let (writer, input) = self.stdio(id).await?;
        // A terminal merges stderr into stdout, so only stdout names the writer.
        let terminal = input.is_some();
        let create = tasks::CreateTaskRequest {
            container_id: id.to_owned(),
            rootfs,
            stdin: input.unwrap_or_default(),
            stderr: if terminal { String::new() } else { writer.clone() },
            stdout: writer,
            terminal,
            ..Default::default()
        };
        self.tasks().create(create).await?;
        let start = tasks::StartRequest { container_id: id.to_owned(), ..Default::default() };
        self.tasks().start(start).await?;
        Ok(())
    }

    /// Sends `signal` to every process of `id`.
    pub async fn signal(&self, id: &str, signal: u32) -> DriverResult<()> {
        let request = tasks::KillRequest {
            container_id: id.to_owned(),
            signal,
            all: true,
            ..Default::default()
        };
        match self.tasks().kill(request).await {
            Ok(_) => Ok(()),
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Waits up to `wait` for the task on `id` to stop; whether it did.
    async fn stopped_within(&self, id: &str, wait: Duration) -> DriverResult<bool> {
        let until = Instant::now() + wait;
        loop {
            match self.task(id).await? {
                None | Some((Status::STOPPED, _, _)) => return Ok(true),
                Some(_) if Instant::now() >= until => return Ok(false),
                Some(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    /// Stops `id` with `signal`, killing it if it outlives `grace`.
    pub async fn stop(&self, id: &str, signal: u32, grace: Duration) -> DriverResult<()> {
        if !matches!(self.task(id).await?, Some((Status::RUNNING | Status::PAUSED, _, _))) {
            return Ok(());
        }
        self.signal(id, signal).await?;
        if self.stopped_within(id, grace).await? {
            return Ok(());
        }
        self.kill(id).await
    }

    /// Kills `id` at once.
    pub async fn kill(&self, id: &str) -> DriverResult<()> {
        self.signal(id, 9).await?;
        if self.stopped_within(id, KILL_WAIT).await? {
            Ok(())
        } else {
            Err(DriverError::Failed(format!("{id} outlived SIGKILL for {KILL_WAIT:?}")))
        }
    }

    /// Releases everything `id` holds: its task, its record, and its snapshot.
    pub async fn destroy(&self, id: &str) -> DriverResult<()> {
        self.kill(id).await?;
        self.delete_task(id).await?;
        let request =
            containers::DeleteContainerRequest { id: id.to_owned(), ..Default::default() };
        match self.containers().delete(request).await {
            Ok(_) => {}
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.remove_snapshot(id).await?;
        self.forget_sample(id);
        if let Some(streams) = &self.streams {
            streams.forget(id)?;
        }
        Ok(())
    }

    /// Every container in the namespace, with its labels.
    pub(crate) async fn list(&self) -> DriverResult<Vec<containers::Container>> {
        let request = containers::ListContainersRequest {
            filters: vec![format!("labels.\"{LABEL_WORKLOAD}\"")],
            ..Default::default()
        };
        Ok(self.containers().list(request).await?.into_owned().containers)
    }
}
