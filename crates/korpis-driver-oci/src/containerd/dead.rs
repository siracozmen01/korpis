// SPDX-License-Identifier: AGPL-3.0-or-later

//! A container whose shim died under it (Finding 163 of docs/23-walkthroughs.md). containerd then
//! forgets the task and cleans up after the shim: it runs the shim's `delete` in the task's
//! bundle, which removes runc's state, and removes the bundle. When that cleanup does not finish,
//! as containerd v2.4.2's did not on this machine, killed past its five seconds (§5 of
//! docs/research/evidence.md), the bundle and runc's state stay, and every later task of the
//! container is refused as existing until containerd itself restarts. So a task the driver creates
//! on a container it holds waits out containerd's cleanup while containerd still holds the task's
//! place, and once containerd has given it up and only the bundle is left, the driver finishes the
//! cleanup as containerd would have, for that container in its own namespace and nothing else.

use super::{Containerd, NAMESPACE};
use crate::error::{DriverError, DriverResult};
use connectrpc::{ConnectError, ErrorCode};
use korpis_containerd::containerd::services::tasks::v1 as tasks;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The shim of the runtime containerd runs containers with, as containerd names its binary.
const SHIM: &str = "containerd-shim-runc-v2";
/// How long the shim's `delete` is given.
const DELETE: Duration = Duration::from_secs(30);
/// How long a task's creation waits out containerd's own cleanup after a dead shim.
const SETTLE: Duration = Duration::from_secs(20);

/// What containerd answers a task's creation while a dead shim's cleanup holds the task's place.
fn still_held(e: &ConnectError) -> bool {
    e.code == ErrorCode::AlreadyExists
        || e.message.as_deref().is_some_and(|m| m.ends_with("already exists"))
}

/// What it answers once the cleanup has given up and left the bundle.
fn bundle_left(e: &ConnectError) -> bool {
    e.message.as_deref().is_some_and(|m| m.ends_with("file exists"))
}

impl Containerd {
    /// Creates the task `create` names, on a container containerd knows no task of: asked
    /// again while a dead shim's cleanup still holds the task's place, and after the driver has
    /// finished a cleanup containerd gave up on.
    ///
    /// # Errors
    /// containerd refused it otherwise, or still after [`SETTLE`], or the cleanup failed.
    pub(crate) async fn create_task(&self, create: tasks::CreateTaskRequest) -> DriverResult<()> {
        let deadline = Instant::now() + SETTLE;
        loop {
            match self.tasks().create(create.clone()).await {
                Ok(_) => return Ok(()),
                Err(e) if Instant::now() >= deadline => return Err(e.into()),
                Err(e) if bundle_left(&e) => self.clear_dead_shim(&create.container_id).await?,
                Err(e) if still_held(&e) => tokio::time::sleep(Duration::from_millis(500)).await,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// The bundle of the task on container `id`, where containerd's runtime keeps it.
    fn bundle(&self, id: &str) -> PathBuf {
        self.state.join("io.containerd.runtime.v2.task").join(NAMESPACE).join(id)
    }

    /// Finishes containerd's cleanup after a dead shim of container `id`, if one left its
    /// bundle: the shim's `delete` run in the bundle, then the bundle removed.
    ///
    /// # Errors
    /// The shim's `delete` failed or did not end in time, or the bundle could not be removed.
    async fn clear_dead_shim(&self, id: &str) -> DriverResult<()> {
        let bundle = self.bundle(id);
        if !bundle.is_dir() {
            return Ok(());
        }
        tracing::warn!(%id, "a dead shim left its task's bundle; cleaning up after it");
        let mut delete = tokio::process::Command::new(SHIM);
        delete.current_dir(&bundle).args(["-namespace", NAMESPACE, "-address"]);
        delete.arg(&self.socket).args(["-id", id, "delete"]).kill_on_drop(true);
        let ended = tokio::time::timeout(DELETE, delete.output()).await.map_err(|_| {
            DriverError::Failed(format!("{SHIM} delete of {id} did not end within {DELETE:?}"))
        })?;
        let ended = ended.map_err(|e| DriverError::Failed(format!("{SHIM} delete: {e}")))?;
        if !ended.status.success() {
            let said = String::from_utf8_lossy(&ended.stderr).into_owned();
            return Err(DriverError::Failed(format!("{SHIM} delete of {id}: {said}")));
        }
        match tokio::fs::remove_dir_all(&bundle).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(DriverError::Failed(format!("{}: {e}", bundle.display())))
            }
            _ => Ok(()),
        }
    }
}
