// SPDX-License-Identifier: AGPL-3.0-or-later

//! A container's life in containerd. `create` imports and unpacks the image under a lease,
//! prepares the container's own snapshot remapped onto the workload's id range, and records the
//! container with its runtime spec and the labels adoption matches it by; nothing runs. `start`
//! runs a task on it, replacing one that exited; `stop` signals, waits out the grace, and kills;
//! `destroy` releases everything the container holds. Volumes are never touched here.

use super::names::{container_id, is_container_id};
use super::{Containerd, NAMESPACE, RUNTIME, SNAPSHOTTER, under};
use crate::devices::Given;
use crate::error::{DriverError, DriverResult};
use crate::oci::runtime_spec;
use crate::validate::{Policy, Valid, Violation};
use buffa_types::google::protobuf::Any;
use korpis_containerd::containerd::services::containers::v1 as containers;
use korpis_containerd::containerd::services::snapshots::v1 as snapshots;
use korpis_containerd::containerd::services::tasks::v1 as tasks;
use korpis_containerd::containerd::v1::types::{Process, Status};
use korpis_driver_kit::cores::Cpuset;
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
pub(crate) const KILL_WAIT: Duration = Duration::from_secs(10);

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
        // Read now, before anything is made, so a device the node no longer describes, or a core
        // the machine does not have, refuses.
        let given = crate::devices::given(s, &policy.devices).map_err(DriverError::Rejected)?;
        let cpuset = self.cores.given(&s.cores).map_err(DriverError::Rejected)?;
        if valid.spec().stream.is_set() {
            let streams = self.streams.as_ref().ok_or_else(|| {
                DriverError::Rejected("this driver has no stream writer configured".into())
            })?;
            streams.write_secrets(valid, &id)?;
            streams.make_input(valid, &id)?;
        }
        let lease = self.lease().await?;
        let made = self.assemble(&lease, (valid, &given, cpuset.as_ref()), &id).await;
        self.release(&lease).await;
        made.map(|()| id)
    }

    /// Imports, unpacks, snapshots, and records the container, all under `lease`, with what its
    /// devices give and its cores' `cpuset`.
    async fn assemble(
        &self,
        lease: &str,
        (valid, given, cpuset): (&Valid, &Given, Option<&Cpuset>),
        id: &str,
    ) -> DriverResult<()> {
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
        let cgroup = self.cores.cgroup(id, cpuset.is_some());
        let spec = runtime_spec(valid, &image.config, &cgroup, given)
            .map_err(|e| DriverError::Rejected(e.to_string()))?
            .with_cpuset(cpuset);
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
        Ok(self
            .process(id)
            .await?
            .map(|p| (p.status.as_known().unwrap_or(Status::UNKNOWN), p.exit_status, p.pid)))
    }

    /// The process of the task on `id`, if there is a task.
    pub(crate) async fn process(&self, id: &str) -> DriverResult<Option<Process>> {
        let request = tasks::GetRequest { container_id: id.to_owned(), ..Default::default() };
        match self.tasks().get(request).await {
            Ok(r) => Ok(Some(r.into_owned().process.into_option().unwrap_or_default())),
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
        self.held(id).await?;
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
        self.create_task(create).await?;
        // A holding container's cgroup is made a partition before anything of it runs.
        if let Err(why) = self.cores.partition(&self.cores.dir(id)) {
            self.delete_task(id).await?;
            return Err(DriverError::Failed(why));
        }
        self.mark_started(id).await?;
        let start = tasks::StartRequest { container_id: id.to_owned(), ..Default::default() };
        self.tasks().start(start).await?;
        Ok(())
    }

    /// Sends `signal` to every process of `id`.
    pub async fn signal(&self, id: &str, signal: u32) -> DriverResult<()> {
        self.held(id).await?;
        self.signal_task(id, signal).await
    }

    /// That the driver holds the container `id`: every verb naming one it does not is answered
    /// `NotFound` (§4.2 of docs/04-runtimes.md).
    async fn held(&self, id: &str) -> DriverResult<()> {
        if self.container_exists(id).await? {
            Ok(())
        } else {
            Err(DriverError::NotFound(id.to_owned()))
        }
    }

    /// Sends the task on `id`, if there is one, `signal`.
    async fn signal_task(&self, id: &str, signal: u32) -> DriverResult<()> {
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
        self.held(id).await?;
        if !matches!(self.task(id).await?, Some((Status::RUNNING | Status::PAUSED, _, _))) {
            return Ok(());
        }
        self.signal_task(id, signal).await?;
        if self.stopped_within(id, grace).await? {
            return Ok(());
        }
        self.kill(id).await
    }

    /// Kills `id` at once.
    pub async fn kill(&self, id: &str) -> DriverResult<()> {
        self.held(id).await?;
        self.signal_task(id, 9).await?;
        if self.stopped_within(id, KILL_WAIT).await? {
            Ok(())
        } else {
            Err(DriverError::Failed(format!("{id} outlived SIGKILL for {KILL_WAIT:?}")))
        }
    }

    /// Releases everything `id` holds: its task, its record, and its snapshot. A container the
    /// driver does not hold is `NotFound`, after what a destroy that stopped part way left of it,
    /// its record gone and its snapshot not, is removed all the same.
    pub async fn destroy(&self, id: &str) -> DriverResult<()> {
        let held = self.held(id).await;
        if held.is_ok() {
            self.kill(id).await?;
            self.delete_task(id).await?;
            let request =
                containers::DeleteContainerRequest { id: id.to_owned(), ..Default::default() };
            match self.containers().delete(request).await {
                Ok(_) => {}
                Err(e) if e.code == connectrpc::ErrorCode::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Only a container's own snapshot and stream state are named as its container is.
        if held.is_ok() || is_container_id(id) {
            self.remove_snapshot(id).await?;
            self.forget_sample(id);
            if let Some(streams) = &self.streams {
                streams.forget(id)?;
            }
        }
        held
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
