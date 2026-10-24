// SPDX-License-Identifier: AGPL-3.0-or-later

//! A command run inside a running container (§4.2 of docs/04-runtimes.md), as containerd's task
//! exec runs one: the container's own process, its user, environment, working directory,
//! capabilities, and limits, with the command in place of the entry, in the container's
//! namespaces and its cgroup. Its stdout and stderr go to one FIFO in the driver's state
//! directory, which the driver opens before the process starts and reads as it is written: what
//! is over the limit is read and dropped, so the command never blocks on a full pipe. A command
//! that outlives its timeout is killed, and the process is deleted from the task however it ended.

use super::{Containerd, lifecycle::KILL_WAIT};
use crate::error::{DriverError, DriverResult};
use buffa_types::google::protobuf::Any;
use korpis_containerd::containerd::services::containers::v1 as containers;
use korpis_containerd::containerd::services::tasks::v1 as tasks;
use korpis_containerd::containerd::v1::types::Status;
use korpis_driver_kit::exec::{Command, Ran};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;

/// The type containerd stores a process spec under.
const PROCESS_TYPE: &str = "types.containerd.io/opencontainers/runtime-spec/1/Process";

/// The process `container`'s spec runs, with `argv` in place of its entry and no terminal.
fn process(container: &containers::Container, argv: &[String]) -> DriverResult<Vec<u8>> {
    let spec = container.spec.as_option().map(|s| &s.value[..]).unwrap_or_default();
    let spec: serde_json::Value =
        serde_json::from_slice(spec).map_err(|e| DriverError::Failed(format!("its spec: {e}")))?;
    let mut process = spec
        .get("process")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .ok_or_else(|| DriverError::Failed("its spec runs no process".into()))?;
    process.insert("args".into(), serde_json::json!(argv));
    process.insert("terminal".into(), serde_json::Value::Bool(false));
    process.remove("consoleSize");
    serde_json::to_vec(&process).map_err(|e| DriverError::Failed(e.to_string()))
}

/// How much more of a command's output is sent, and whether any was dropped.
#[derive(Debug)]
struct Budget {
    left: usize,
    truncated: bool,
}

impl Budget {
    /// Sends what of `read` is within the budget on `out`; whether `out` still takes it.
    async fn send(&mut self, read: &[u8], out: &mpsc::Sender<Ran>) -> bool {
        let sent = read.get(..read.len().min(self.left)).unwrap_or_default();
        self.truncated |= sent.len() < read.len();
        self.left -= sent.len();
        sent.is_empty() || out.send(Ran::Output(sent.to_vec())).await.is_ok()
    }
}

/// Reads what the command writes on `reader` as it comes, and sends it within `budget`, until
/// the FIFO fails or `out` is gone; a FIFO opened for writing too never ends on its own.
async fn forward(
    reader: &tokio::net::unix::pipe::Receiver,
    out: &mpsc::Sender<Ran>,
    budget: &mut Budget,
) {
    let mut buffer = vec![0_u8; 8192];
    while reader.readable().await.is_ok() {
        match reader.try_read(&mut buffer) {
            Ok(n) if n > 0 => {
                if !budget.send(buffer.get(..n).unwrap_or_default(), out).await {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            _ => return,
        }
    }
}

/// What is left on `reader` once the command has ended, sent within `budget`.
async fn drain(
    reader: &tokio::net::unix::pipe::Receiver,
    out: &mpsc::Sender<Ran>,
    budget: &mut Budget,
) {
    let mut buffer = vec![0_u8; 8192];
    while let Ok(n) = reader.try_read(&mut buffer) {
        if n == 0 || !budget.send(buffer.get(..n).unwrap_or_default(), out).await {
            return;
        }
    }
}

type Waited = tokio::task::JoinHandle<Result<u32, connectrpc::ConnectError>>;

impl Containerd {
    /// Runs `c` in the running task of container `id`, sending what it writes and how it ended
    /// on `out`.
    ///
    /// # Errors
    /// The container does not run, the driver has no state directory, or containerd failed.
    pub async fn exec(&self, id: &str, c: Command, out: mpsc::Sender<Ran>) -> DriverResult<()> {
        if !matches!(self.task(id).await?, Some((Status::RUNNING, _, _))) {
            return Err(DriverError::Rejected(format!("{id} does not run")));
        }
        let request = containers::GetContainerRequest { id: id.to_owned(), ..Default::default() };
        let container = self.containers_client().get(request).await?.into_owned().container;
        let container = container.into_option().ok_or_else(|| DriverError::NotFound(id.into()))?;
        let process = process(&container, c.argv())?;
        let streams = self.streams.as_ref().ok_or_else(|| {
            DriverError::Rejected(
                "this driver has no state directory for a command's output".into(),
            )
        })?;
        let exec = format!("exec-{}", uuid::Uuid::now_v7().simple());
        let fifo = fifo(&streams.state, &exec)?;
        let ran = self.run(id, (&exec, &fifo), process, &c, &out).await;
        let delete = tasks::DeleteProcessRequest {
            container_id: id.to_owned(),
            exec_id: exec.clone(),
            ..Default::default()
        };
        if let Err(e) = self.tasks().delete_process(delete).await
            && e.code != connectrpc::ErrorCode::NotFound
        {
            tracing::warn!(%id, %exec, error = %e, "a command's process could not be deleted");
        }
        let _ = std::fs::remove_file(&fifo);
        ran
    }

    /// Starts `process` as `exec` in container `id`, its output on `fifo`, and sends what it
    /// writes and how it ended.
    async fn run(
        &self,
        id: &str,
        (exec, fifo): (&str, &Path),
        process: Vec<u8>,
        c: &Command,
        out: &mpsc::Sender<Ran>,
    ) -> DriverResult<()> {
        // Opened for reading and writing, so it neither waits for the shim nor ends before it.
        let reader = tokio::net::unix::pipe::OpenOptions::new()
            .read_write(true)
            .open_receiver(fifo)
            .map_err(|e| DriverError::Failed(format!("{}: {e}", fifo.display())))?;
        let path = fifo.to_string_lossy().into_owned();
        let request = tasks::ExecProcessRequest {
            container_id: id.to_owned(),
            stdout: path.clone(),
            stderr: path,
            spec: Any {
                type_url: PROCESS_TYPE.into(),
                value: process.into(),
                ..Default::default()
            }
            .into(),
            exec_id: exec.to_owned(),
            ..Default::default()
        };
        self.tasks().exec(request).await?;
        // Waited for before it starts, so an exit as it starts is not missed.
        let (client, ids) = (self.tasks(), (id.to_owned(), exec.to_owned()));
        let waited: Waited = tokio::spawn(async move {
            let (container_id, exec_id) = ids;
            let wait = tasks::WaitRequest { container_id, exec_id, ..Default::default() };
            client.wait(wait).await.map(|w| w.into_owned().exit_status)
        });
        let start = tasks::StartRequest {
            container_id: id.to_owned(),
            exec_id: exec.to_owned(),
            ..Default::default()
        };
        if let Err(e) = self.tasks().start(start).await {
            waited.abort();
            return Err(e.into());
        }
        let mut budget = Budget { left: c.output_limit(), truncated: false };
        let ended = self.follow(id, exec, (&reader, out, &mut budget), waited, c.timeout()).await;
        let (status, timed_out) = ended?;
        drain(&reader, out, &mut budget).await;
        let exit = Ran::Exit { status, timed_out, truncated: budget.truncated };
        let _ = out.send(exit).await;
        Ok(())
    }

    /// Forwards the output of the process `exec` of container `id` until it ends, killing it once
    /// it outlives `timeout`; its status, and whether it was killed for its timeout.
    async fn follow(
        &self,
        id: &str,
        exec: &str,
        (reader, out, budget): (&tokio::net::unix::pipe::Receiver, &mpsc::Sender<Ran>, &mut Budget),
        mut waited: Waited,
        timeout: Duration,
    ) -> DriverResult<(u32, bool)> {
        let reading = forward(reader, out, budget);
        tokio::pin!(reading);
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let (mut read, mut timed_out) = (false, false);
        loop {
            tokio::select! {
                status = &mut waited => {
                    let status = status.map_err(|e| DriverError::Failed(format!("waiting: {e}")))?;
                    return Ok((status?, timed_out));
                }
                () = &mut reading, if !read => read = true,
                () = &mut deadline => {
                    if timed_out {
                        waited.abort();
                        let why = format!("a command in {id} outlived SIGKILL for {KILL_WAIT:?}");
                        return Err(DriverError::Failed(why));
                    }
                    timed_out = true;
                    self.kill_process(id, exec).await;
                    deadline.as_mut().reset(tokio::time::Instant::now() + KILL_WAIT);
                }
            }
        }
    }

    /// Kills the process `exec` of container `id`.
    async fn kill_process(&self, id: &str, exec: &str) {
        let kill = tasks::KillRequest {
            container_id: id.to_owned(),
            exec_id: exec.to_owned(),
            signal: 9,
            ..Default::default()
        };
        if let Err(e) = self.tasks().kill(kill).await {
            tracing::warn!(%id, %exec, error = %e, "a command past its timeout could not be killed");
        }
    }
}

/// Makes the FIFO a command `exec` writes to, in the driver's state directory, readable by the
/// driver's user alone (and by the shim, which runs as root).
fn fifo(state: &Path, exec: &str) -> DriverResult<PathBuf> {
    let dir = state.join("exec");
    let io = |e: std::io::Error| DriverError::Failed(format!("{}: {e}", dir.display()));
    std::fs::create_dir_all(&dir).map_err(io)?;
    let path = dir.join(exec);
    korpis_driver_kit::console::make_fifo(&path).map_err(io)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn the_process_is_the_containers_own_with_the_command_in_place_of_its_entry() {
        let spec = serde_json::json!({
            "process": {
                "args": ["/entry"], "terminal": true, "consoleSize": {"height": 1, "width": 1},
                "user": {"uid": 1000, "gid": 1000}, "env": ["A=1"], "cwd": "/srv",
                "noNewPrivileges": true
            }
        });
        let container = containers::Container {
            spec: Any { value: serde_json::to_vec(&spec).unwrap().into(), ..Default::default() }
                .into(),
            ..Default::default()
        };
        let argv = vec!["pg_isready".to_owned(), "-q".to_owned()];
        let process: serde_json::Value =
            serde_json::from_slice(&process(&container, &argv).unwrap()).unwrap();
        assert_eq!(process["args"], serde_json::json!(["pg_isready", "-q"]));
        assert_eq!(process["terminal"], false);
        assert_eq!(process["user"]["uid"], 1000, "as its own user");
        assert_eq!(process["env"], serde_json::json!(["A=1"]));
        assert!(process.get("consoleSize").is_none());
    }
}
