// SPDX-License-Identifier: AGPL-3.0-or-later

//! Stopping, signalling, killing, and asking a guest (§6 of docs/04-runtimes.md). A signal named
//! reaches the entry through the guest's agent on the control port, and a stop the agent does not
//! take reaches it as Ctrl+Alt+Del, which the guest's first process turns into `SIGTERM` for the
//! entry's group; a kill is Ctrl+Alt+Del sent until the guest has ended, then its Firecracker
//! killed past a bound.

use super::{AGAIN, KILL_BOUND, Machine, MachineError, MachineResult, failed};
use crate::object::Object;
use crate::{agent, api, console, jail, process};
use korpis_guest::control::{self, Resized, Signalled};
use std::time::{Duration, Instant};

/// How long a guest's agent is given to take a signal.
const ASK: Duration = Duration::from_secs(5);

impl Machine {
    /// Tells object `id`'s guest to stop, as the driver's graceful default, and kills it once
    /// `grace` has passed if it has not ended.
    pub async fn stop(&self, id: &str, signal: &str, grace: Duration) -> MachineResult<()> {
        self.signal(id, signal).await?;
        let machine = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let _ = machine.kill(&id).await;
        });
        Ok(())
    }

    /// Sends the entry of object `id`'s guest the signal `signal` names, `SIGTERM` where it names
    /// none, through the guest's agent (§6 of docs/04-runtimes.md). A stop the agent does not
    /// take is sent as Ctrl+Alt+Del instead, which reaches the entry as `SIGTERM`; any other
    /// signal it does not take is an error. A guest not running has nothing to signal.
    pub async fn signal(&self, id: &str, signal: &str) -> MachineResult<()> {
        let name = if signal.is_empty() { "SIGTERM" } else { signal };
        if !control::SIGNALS.contains(&name) {
            return Err(MachineError::Refused(format!("{name}: not a signal a guest is sent")));
        }
        if !self.load(id)?.vm.as_ref().is_some_and(process::alive) {
            return Ok(());
        }
        self.until_started(id).await?;
        match agent::signal(&jail::vsock_socket(&self.settings, id), name, ASK).await {
            Ok(Signalled::Sent | Signalled::NoEntry) => Ok(()),
            Ok(Signalled::Unknown) => Err(MachineError::Refused(format!(
                "{name}: the guest's agent sends no such signal"
            ))),
            Err(e) if name == "SIGTERM" => {
                tracing::info!(%id, error = %e, "a stop goes to the guest as Ctrl+Alt+Del");
                self.ctrl_alt_del(id).await
            }
            Err(e) => {
                Err(MachineError::Failed(format!("the guest's agent did not take {name}: {e}")))
            }
        }
    }

    /// Sends object `id`'s guest Ctrl+Alt+Del, the stop a guest can always be sent, once in a
    /// run: a second would be read as its kill.
    async fn ctrl_alt_del(&self, id: &str) -> MachineResult<()> {
        let object = self.load(id)?;
        let dir = self.dir(id)?;
        if !object.vm.as_ref().is_some_and(process::alive) || !Object::tell(&dir).map_err(failed)? {
            return Ok(());
        }
        let told = api::ctrl_alt_del(&jail::api_socket(&self.settings, id)).await;
        if told.is_err() {
            let _ = Object::untell(&dir);
        }
        told.map_err(failed)
    }

    /// Asks the agent of object `id`'s guest whether it answers within `within`; why not, where it
    /// does not. A guest that does not run has no agent to ask, and is refused.
    pub async fn ping_guest(
        &self,
        id: &str,
        within: Duration,
    ) -> MachineResult<Result<(), String>> {
        if !self.load(id)?.vm.as_ref().is_some_and(process::alive) {
            return Err(MachineError::Refused(format!("{id} does not run")));
        }
        let pinged = agent::ping(&jail::vsock_socket(&self.settings, id), within).await;
        Ok(pinged.map(drop).map_err(|e| e.to_string()))
    }

    /// Kills object `id`'s guest: a second Ctrl+Alt+Del, which ends every process and has the
    /// guest sync and restart, then its Firecracker if it has not ended within [`KILL_BOUND`].
    pub async fn kill(&self, id: &str) -> MachineResult<()> {
        let object = self.load(id)?;
        let Some(vm) = object.vm.filter(process::alive) else { return Ok(()) };
        // Sent until the guest has ended: a kill with no stop before it is the guest's first, and
        // a Ctrl+Alt+Del that arrives while the guest has not yet read the last one is absorbed by
        // it, the kernel keeping one pending signal of a kind.
        let socket = jail::api_socket(&self.settings, id);
        let until = Instant::now() + KILL_BOUND;
        while process::alive(&vm) && Instant::now() < until {
            let _ = api::ctrl_alt_del(&socket).await;
            tokio::time::sleep(AGAIN).await;
        }
        process::kill(&vm);
        let until = Instant::now() + KILL_BOUND;
        while process::alive(&vm) && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    /// Resizes the terminal of object `id`'s interactive entry to `(cols, rows)`, recording in its
    /// stream first that `from` (who, and in which session) did, as a container's is recorded;
    /// the guest's agent sets the size, and the kernel tells the entry with `SIGWINCH`.
    pub async fn resize_console(
        &self,
        id: &str,
        from: (&str, &str),
        size: (u16, u16),
    ) -> MachineResult<()> {
        let object = self.load(id)?;
        if !object.vm.as_ref().is_some_and(process::alive) {
            return Err(MachineError::Refused(format!("{id} does not run")));
        }
        let dir = self.dir(id)?;
        self.until_started(id).await?;
        console::record_resize(&dir, object.spec.stream.as_option(), from, size).map_err(
            |(refused, why)| {
                if refused { MachineError::Refused(why) } else { MachineError::Failed(why) }
            },
        )?;
        match agent::resize(&jail::vsock_socket(&self.settings, id), size, ASK).await {
            Ok(Resized::Set) => Ok(()),
            Ok(Resized::NoTerminal) => {
                Err(MachineError::Refused("the guest's entry has no terminal".into()))
            }
            Ok(Resized::Failed) => {
                Err(MachineError::Failed("the guest's terminal would not take the size".into()))
            }
            Err(e) => Err(MachineError::Failed(format!("the guest's agent did not take it: {e}"))),
        }
    }
}
