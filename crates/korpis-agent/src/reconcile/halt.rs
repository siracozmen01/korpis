// SPDX-License-Identifier: AGPL-3.0-or-later

//! Stopping a runtime object without the reconciler waiting in the call (§6 of
//! docs/02-architecture.md). The reconciler moves through the node's workloads one after another,
//! so a stop it waited in, a grace of thirty seconds or a hook's minutes, would hold up every
//! other workload on the node for as long. A stop is instead a state kept with what it stops:
//! each pass moves it on (the object is signalled, then killed once its grace is over) and says
//! whether it has stopped, and whatever needed it stopped carries on in the pass that finds it
//! so. The process's exit is a change the driver reports, which wakes the reconciler at once; a
//! kill waits for the next pass after the grace, at most one period late.

use super::Reconciler;
use super::act::{Failure, running};
use crate::host::Host;
use crate::runtime::{Runtime, RuntimeError};
use std::time::Instant;

/// Where the stop of one object is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Halting {
    /// When it was signalled to stop, if it has been.
    signalled: Option<Instant>,
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Moves the stop of `id` on by one pass, as `h` says it stands: signals it if it has not
    /// been, and kills it once its grace is over; whether it has stopped. An object that is
    /// gone, or not running, has.
    pub(super) async fn halt(&self, id: &str, h: &mut Halting) -> Result<bool, Failure> {
        if !self.running_now(id).await? {
            *h = Halting::default();
            return Ok(true);
        }
        match h.signalled {
            None => {
                self.runtime.signal(id, "").await?;
                h.signalled = Some(Instant::now());
            }
            Some(at) if at.elapsed() >= self.wiring.grace => self.runtime.kill(id).await?,
            Some(_) => {}
        }
        // A process that exits on the signal has stopped in this same pass.
        let stopped = !self.running_now(id).await?;
        if stopped {
            *h = Halting::default();
        }
        Ok(stopped)
    }

    /// Whether `id` is running now; an object that is gone is not.
    async fn running_now(&self, id: &str) -> Result<bool, Failure> {
        match self.runtime.inspect(id).await {
            Ok(s) => Ok(running(&s)),
            Err(RuntimeError::NotFound(_)) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}
