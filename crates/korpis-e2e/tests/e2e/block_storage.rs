// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class on real nodes (§2.1 of docs/06-storage.md, trace 81 of
//! docs/23-walkthroughs.md). Two nodes offer the class `fast`, whose devices are files in one
//! directory both reach, as two machines reach one storage, attached through loop devices; its
//! provider is the test's own, served from the extension's workload on the second node, and keeps
//! the files, refusing an attach for an epoch older than the one it holds. A workload whose volume
//! is in the class runs on the first node and writes to it, through the device the helper
//! formatted and mounted; when that node loses its power, the workload runs on the second once its
//! lease has ended, its device attached there for the new lease's epoch and holding what the first
//! wrote. Retired, its device is let go of on the node. Needs root, containerd, and a loop device.

use crate::helper::{Storage, block_root};
use crate::mesh::cordon;
use crate::node::{Node, live};
use crate::recipes::{push_block_storage, push_sleeper};
use crate::shared_storage::{lines_reach, uncordon, until_offering};
use crate::support::{ControlPlane, mutation};
use korpis_extension::block::{BlockStorage, Failure, Storage as Served};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, IdentitySocket};
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::future::{Future, ready};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The storage, as the test's provider keeps it: a file per device in `dir`, and the node and
/// epoch each is attached for.
#[derive(Clone)]
struct LoopDisks {
    dir: PathBuf,
    held: Arc<Mutex<BTreeMap<String, (String, u64)>>>,
}

fn refused(reason: &str) -> Failure {
    Failure { reason: reason.into(), message: String::new(), retryable: false }
}

impl LoopDisks {
    fn holder(&self, device: &str) -> Option<(String, u64)> {
        self.held.lock().unwrap().get(device).cloned()
    }

    fn create_now(&self, c: &pb::CreateBlockVolume) -> Result<String, Failure> {
        let device = format!("{}.img", c.volume);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(&device))
            .map_err(|_| refused("block.unwritable"))?;
        if file.metadata().map_err(|_| refused("block.unreadable"))?.len() == 0 {
            file.set_len(c.size_bytes).map_err(|_| refused("block.unwritable"))?;
        }
        Ok(device)
    }

    fn attach_now(&self, a: &pb::AttachBlockVolume) -> Result<pb::BlockAttachment, Failure> {
        if !self.dir.join(&a.device).exists() {
            return Err(refused("block.unknown"));
        }
        let mut held = self.held.lock().map_err(|_| refused("block.poisoned"))?;
        if held.get(&a.device).is_some_and(|(_, e)| *e > a.epoch) {
            return Err(refused("block.epoch_superseded"));
        }
        held.insert(a.device.clone(), (a.node.clone(), a.epoch));
        Ok(pb::BlockAttachment {
            protocol: pb::BlockProtocol::BLOCK_PROTOCOL_LOOP.into(),
            target: a.device.clone(),
            ..Default::default()
        })
    }
}

impl BlockStorage for LoopDisks {
    fn create(
        &self,
        c: pb::CreateBlockVolume,
    ) -> impl Future<Output = Result<String, Failure>> + Send {
        ready(self.create_now(&c))
    }

    fn attach(
        &self,
        a: pb::AttachBlockVolume,
    ) -> impl Future<Output = Result<pb::BlockAttachment, Failure>> + Send {
        ready(self.attach_now(&a))
    }

    fn detach(&self, d: pb::DetachBlockVolume) -> impl Future<Output = Result<(), Failure>> + Send {
        let mut held = self.held.lock().unwrap();
        if held.get(&d.device).is_some_and(|(n, _)| *n == d.node) {
            held.remove(&d.device);
        }
        ready(Ok(()))
    }

    fn delete(&self, device: String) -> impl Future<Output = Result<(), Failure>> + Send {
        let _ = std::fs::remove_file(self.dir.join(device));
        ready(Ok(()))
    }

    fn describe(
        &self,
        device: String,
    ) -> impl Future<Output = Result<pb::BlockVolumeDescription, Failure>> + Send {
        let size = std::fs::metadata(self.dir.join(&device)).map(|m| m.len());
        let (node, epoch) = self.holder(&device).unwrap_or_default();
        ready(size.map_err(|_| refused("block.unknown")).map(|size_bytes| {
            pb::BlockVolumeDescription { size_bytes, node, epoch, ..Default::default() }
        }))
    }
}

/// Installs the block storage extension in `project`; its workload.
async fn installed(cp: &ControlPlane, project: &str) -> String {
    let request = pb::InstallExtensionRequest {
        mutation: mutation("install-block").into(),
        artifact: push_block_storage().await,
        version: crate::recipes::VERSION.into(),
        organization: cp.root_org.clone(),
        workload_project: project.to_owned(),
        ..Default::default()
    };
    let done = pb::ExtensionServiceClient::new(cp.http(), cp.config())
        .install_extension(request)
        .await
        .unwrap()
        .into_owned();
    done.extension.into_option().unwrap().workload
}

/// Serves `disks` as the extension's workload, from its identity socket at `socket`.
async fn serve(socket: PathBuf, disks: LoopDisks) {
    for _ in 0..300 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(socket.exists(), "the extension's workload never ran: {}", socket.display());
    let connection = Arc::new(Connection::new(IdentitySocket::at(socket)));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_BLOCK_STORAGE,
        name: "fast".into(),
        version: crate::recipes::VERSION.into(),
        concurrency: 4,
        sign_in: false,
        fencing: pb::Fencing::FENCING_STORAGE_ENFORCED,
    };
    let provider = Arc::new(Provider::new(connection, registration, Served(disks)));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    let within =
        tokio::time::timeout(Duration::from_secs(120), registered.wait_for(Option::is_some));
    within.await.expect("the provider registers within two minutes").unwrap();
}

/// A node named `name` offering the block class `fast` on the files in `disks`; its block root.
async fn join_block(cp: &ControlPlane, name: &str, disks: &Path) -> (Node, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let (volumes, root) = (dir.path().join("volumes"), block_root(&dir.path().join("helper")));
    let block = Some(Storage::Block(disks.to_owned()));
    (Node::join_with(cp, name, dir, (volumes, None, block), Vec::new(), None).await, root)
}

/// Whether a loop device is backed by a file in `disks`.
fn looped(disks: &Path) -> Option<String> {
    let entries = std::fs::read_dir("/sys/block").ok()?;
    entries.filter_map(Result::ok).find_map(|e| {
        let backing = std::fs::read_to_string(e.path().join("loop/backing_file")).ok()?;
        Path::new(backing.trim_end())
            .starts_with(disks)
            .then(|| format!("/dev/{}", e.file_name().to_string_lossy()))
    })
}

/// Unmounts whatever is mounted beneath the block root `root` and clears every loop device
/// backed by a file in `disks`: what a machine that lost its power holds no more, and what a test
/// that failed leaves for no other.
fn cut(root: &Path, disks: &Path) {
    for dir in std::fs::read_dir(root.join("fast")).into_iter().flatten().filter_map(Result::ok) {
        let _ = Command::new("umount").arg(dir.path()).status();
    }
    while let Some(device) = looped(disks) {
        if !Command::new("losetup").arg("-d").arg(&device).status().is_ok_and(|s| s.success()) {
            break;
        }
    }
}

/// Cuts the nodes' block roots when the test ends, however it ends.
struct Cut(Vec<PathBuf>, PathBuf);

impl Drop for Cut {
    fn drop(&mut self) {
        for root in &self.0 {
            cut(root, &self.1);
        }
    }
}

#[tokio::test]
async fn a_workload_on_a_block_class_fails_over_holding_what_it_wrote() {
    if !live() || !Path::new("/dev/loop-control").exists() {
        return;
    }
    let cp = ControlPlane::start_beating(Duration::from_secs(3)).await;
    let disks = tempfile::tempdir().unwrap();
    let ((a, root_a), (b, root_b)) = (
        join_block(&cp, "blk-a", disks.path()).await,
        join_block(&cp, "blk-b", disks.path()).await,
    );
    let _cut = Cut(vec![root_a.clone(), root_b.clone()], disks.path().to_owned());
    let (agent_a, agent_b) = (a.agent().await, b.agent().await);
    let named = |n: &Node| n.state.load().unwrap().node;
    let (na, nb) = (named(&a), named(&b));
    for n in [&na, &nb] {
        until_offering(&cp, n, "fast").await;
    }
    let project = cp.project().await;
    // The provider runs on the node that keeps its power.
    cordon(&cp, &na).await;
    let extension = installed(&cp, &project).await;
    b.guard(&extension);
    let socket = b.state.root().join("identity").join(&extension).join("identity.sock");
    let provider = LoopDisks { dir: disks.path().to_owned(), held: Arc::default() };
    serve(socket, provider.clone()).await;
    uncordon(&cp, &na).await;
    cordon(&cp, &nb).await;
    let spec = pb::VolumeSpec {
        name: "data".into(),
        storage_class: "fast".into(),
        size_bytes: 64 << 20,
        ..Default::default()
    };
    let db = cp.declare_with(&project, "db", &push_sleeper().await, vec![spec]).await;
    a.guard(&db);
    b.guard(&db);
    let on = |root: &Path| root.join("fast").join(format!("{db}.data")).join("starts");
    lines_reach((&cp, &db), &on(&root_a), 1).await;
    let device = provider.held.lock().unwrap().keys().next().cloned().unwrap();
    let (first, epoch) = provider.holder(&device).unwrap();
    assert_eq!(first, na, "attached to the node it runs on");
    uncordon(&cp, &nb).await;

    // The power is cut: its agent ends without a word, and what it ran and mounted ends with it.
    drop(agent_a);
    crate::containers::remove_workload(&db);
    cut(&root_a, disks.path());
    let lines = lines_reach((&cp, &db), &on(&root_b), 2).await;
    assert_eq!(lines.len(), 2, "started again, on the other node, on what the first wrote");
    let (now, later) = provider.holder(&device).unwrap();
    assert_eq!(now, nb, "attached to the node it runs on now");
    assert!(later > epoch, "for the new lease's epoch, which the storage fences the old by");
    let placed = cp.status(&db).await.placement.into_option().unwrap();
    assert_eq!(placed.bound_by.as_known(), Some(pb::BoundBy::BOUND_BY_FAILOVER));

    cp.retire(&db).await;
    for _ in 0..300 {
        if looped(disks.path()).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(looped(disks.path()), None, "let go of once nothing of it runs");
    agent_b.stop().await;
}
