// SPDX-License-Identifier: Apache-2.0

//! The driver's process, as the suite runs it: started as the fixture says, serving once its
//! socket takes a connection, killed with `SIGKILL` as a crash ends it, and started again; and
//! what a restart of the host does to its objects, run between the two. It is killed when the
//! suite is done with it.

use crate::fixture::{Fixture, Launch};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a driver is given to serve once started.
const SERVING: Duration = Duration::from_secs(60);

/// The driver's process, while the suite runs it.
#[derive(Debug, Default)]
pub struct Process(Mutex<Option<Child>>);

/// Starts `launch`, with `extra` arguments after its own.
fn spawn(launch: &Launch, extra: &[String]) -> Result<Child, String> {
    Command::new(&launch.program)
        .args(&launch.args)
        .args(extra)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("starting {}: {e}", launch.program.display()))
}

/// Waits until `socket` takes a connection.
async fn serving(socket: &Path) -> Result<(), String> {
    let deadline = Instant::now() + SERVING;
    while std::os::unix::net::UnixStream::connect(socket).is_err() {
        if Instant::now() >= deadline {
            return Err(format!(
                "the driver did not serve on {} within {SERVING:?}",
                socket.display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

impl Process {
    /// Starts the driver `fixture` names and waits until it serves.
    ///
    /// # Errors
    /// It could not be started, or did not serve in time.
    pub async fn start(&self, fixture: &Fixture) -> Result<(), String> {
        let child = spawn(&fixture.driver, &[])?;
        if let Ok(mut held) = self.0.lock() {
            *held = Some(child);
        }
        serving(&fixture.socket).await
    }

    /// Kills the driver with `SIGKILL`, as a crash ends it, and reaps it.
    ///
    /// # Errors
    /// It could not be killed.
    pub fn kill(&self) -> Result<(), String> {
        let child = self.0.lock().map(|mut held| held.take()).map_err(|_| "poisoned")?;
        if let Some(mut child) = child {
            child.kill().map_err(|e| format!("killing the driver: {e}"))?;
            child.wait().map_err(|e| format!("reaping the driver: {e}"))?;
        }
        Ok(())
    }

    /// Runs `restart` with each of `pids` as a further argument, to its end.
    ///
    /// # Errors
    /// It could not be run, or failed.
    pub fn restart_host(restart: &Launch, pids: &[u32]) -> Result<(), String> {
        let pids: Vec<String> = pids.iter().map(ToString::to_string).collect();
        let ended =
            spawn(restart, &pids)?.wait().map_err(|e| format!("the host's restart: {e}"))?;
        if ended.success() { Ok(()) } else { Err(format!("the host's restart ended {ended}")) }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}
