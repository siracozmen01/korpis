// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the in-memory runtime does as a guest would: a signal or a stop hook heeded or not, a
//! command run, and a guest's agent answering.

use super::{InMemory, state};
use crate::runtime::{Executed, RuntimeError, RuntimeResult};
use korpis_proto::korpis::driver::v1 as driver;

impl InMemory {
    /// What a stop hook writes (the session `pre_stop`) stops a running object, as the software
    /// a recipe's hook is written for exits on it, unless the runtime was made to ignore it.
    pub(super) fn heed(&self, id: &str, session: &str) -> RuntimeResult<()> {
        let mut s = self.lock()?;
        let unheeding = s.unheeding;
        let Some(o) = s.objects.get_mut(id) else { return Ok(()) };
        let running =
            o.status.state.as_known() == Some(driver::RuntimeState::RUNTIME_STATE_RUNNING);
        if session != "pre_stop" || unheeding || !running {
            return Ok(());
        }
        o.status = state(driver::RuntimeState::RUNTIME_STATE_STOPPED, Some(0));
        drop(s);
        let _ = self.changes.send(id.to_owned());
        Ok(())
    }

    /// A terminating signal stops a running object, as a process that handles it by exiting
    /// does, unless the runtime was made to ignore it; any other is only recorded.
    pub(super) fn signal_now(&self, id: &str, signal: &str) -> RuntimeResult<()> {
        let mut s = self.call("signal", id)?;
        let deaf = s.deaf;
        let o = s.objects.get_mut(id).ok_or_else(|| RuntimeError::NotFound(id.into()))?;
        let running =
            o.status.state.as_known() == Some(driver::RuntimeState::RUNTIME_STATE_RUNNING);
        if !matches!(signal, "" | "SIGTERM" | "SIGINT") || deaf || !running {
            return Ok(());
        }
        o.status = state(driver::RuntimeState::RUNTIME_STATE_STOPPED, Some(0));
        drop(s);
        let _ = self.changes.send(id.to_owned());
        Ok(())
    }

    /// Records the command, and ends it as [`InMemory::exec_status`] says, writing nothing; a
    /// command is run only in an object that runs.
    pub(super) fn exec_now(&self, id: &str) -> RuntimeResult<Executed> {
        let s = self.call("exec", id)?;
        let o = s.objects.get(id).ok_or_else(|| RuntimeError::NotFound(id.into()))?;
        if o.status.state.as_known() != Some(driver::RuntimeState::RUNTIME_STATE_RUNNING) {
            return Err(RuntimeError::Failed(format!("{id} does not run")));
        }
        Ok(Executed { status: s.exec_status, ..Executed::default() })
    }

    /// Answers as [`InMemory::guest_silent`] says, for an object that runs.
    pub(super) fn ping_now(&self, id: &str) -> RuntimeResult<bool> {
        let s = self.call("ping_guest", id)?;
        let o = s.objects.get(id).ok_or_else(|| RuntimeError::NotFound(id.into()))?;
        if o.status.state.as_known() != Some(driver::RuntimeState::RUNTIME_STATE_RUNNING) {
            return Err(RuntimeError::Failed(format!("{id} does not run")));
        }
        Ok(!s.silent)
    }
}
