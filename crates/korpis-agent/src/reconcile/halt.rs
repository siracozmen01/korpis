// SPDX-License-Identifier: AGPL-3.0-or-later

//! Stopping a runtime object without the reconciler waiting in the call (§6 of
//! docs/02-architecture.md). The reconciler moves through the node's workloads one after another,
//! so a stop it waited in, a grace of thirty seconds or a hook's minutes, would hold up every
//! other workload on the node for as long. A stop is instead a state kept with what it stops:
//! each pass moves it on and says whether it has stopped, and whatever needed it stopped carries
//! on in the pass that finds it so. The process's exit is a change the driver reports, which
//! wakes the reconciler at once; anything timed waits for the next pass, at most one period late.
//!
//! A stop asks first when the recipe says how (§3 of docs/09-recipes.md): its `pre_stop` hook's
//! data is written to the console as korpis's input, recorded in the stream as the reconciler's,
//! and the process is given the hook's `wait_for_exit` to exit by itself. Only then is it
//! signalled, and killed once its grace is over.

use super::act::{Failure, running};
use super::{Reconciler, Tracked, effect, text};
use crate::host::Host;
use crate::runtime::{Runtime, RuntimeError, Sender};
use korpis_core::recipe::behaviour::Hook;
use korpis_proto::korpis::agent::v1 as agent;
use std::time::Instant;

/// Who the hook's input is recorded as: the node acting for the control plane.
const ASKER: &str = "system:reconciler";

/// Where the stop of one object is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Halting {
    /// When the recipe's hook asked it to stop, if it has.
    asked: Option<Instant>,
    /// When it was signalled to stop, if it has been.
    signalled: Option<Instant>,
}

/// The pre-stop hook of `a`'s recipe, read into `t` once for each recipe it is assigned.
pub(super) fn hook(a: &agent::Assignment, t: &mut Tracked) {
    let target = a.target.as_option().and_then(|i| i.body.as_option());
    let digest = target.and_then(|b| b.recipe.as_option()).map(|r| r.digest.as_str());
    let digest = digest.unwrap_or_default();
    if t.hook_of == digest {
        return;
    }
    digest.clone_into(&mut t.hook_of);
    t.pre_stop = match korpis_recipe::parse(&a.recipe_manifest) {
        Ok(recipe) => recipe.lifecycle_hooks.pre_stop,
        Err(e) => {
            tracing::warn!(workload = %a.workload, error = %e, "the recipe's hooks are unread");
            None
        }
    };
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Moves the stop of `id` on by one pass, as `h` says it stands: asks it as `hook` says
    /// first, for workload `workload`, then signals it once the hook's wait is over, and kills
    /// it once its grace is; whether it has stopped. An object that is gone, or not running, has.
    pub(super) async fn halt(
        &self,
        id: &str,
        h: &mut Halting,
        hook: Option<(&str, &Hook)>,
    ) -> Result<bool, Failure> {
        if !self.running_now(id).await? {
            *h = Halting::default();
            return Ok(true);
        }
        let now = Instant::now();
        match (hook, h.asked, h.signalled) {
            (Some((workload, Hook::ConsoleWrite { data, .. })), None, None) => {
                h.asked = Some(now);
                if !self.ask(workload, id, data).await {
                    self.runtime.signal(id, "").await?;
                    h.signalled = Some(now);
                }
            }
            (Some((_, Hook::ConsoleWrite { wait_for_exit, .. })), Some(at), None)
                if at.elapsed() < *wait_for_exit => {}
            (_, _, None) => {
                self.runtime.signal(id, "").await?;
                h.signalled = Some(now);
            }
            (_, _, Some(at)) if at.elapsed() >= self.wiring.grace => self.runtime.kill(id).await?,
            (_, _, Some(_)) => {}
        }
        // A process that exits on what it was told has stopped in this same pass.
        let stopped = !self.running_now(id).await?;
        if stopped {
            *h = Halting::default();
        }
        Ok(stopped)
    }

    /// Writes the hook's `data` to `id`'s console as the reconciler's input, and journals that
    /// it did, or why it could not; whether it reached the workload. A workload whose console
    /// takes no input cannot be asked, and is signalled at once.
    async fn ask(&self, workload: &str, id: &str, data: &str) -> bool {
        let from = Sender { by: ASKER, session: "pre_stop" };
        let written = self.runtime.console_input(id, from, data.as_bytes()).await;
        let mut e = effect(Some(workload), "node.pre_stop", written.is_ok());
        e.after.insert("runtime_id".into(), text(id));
        e.after.insert("bytes".into(), text(&data.len().to_string()));
        if let Err(why) = &written {
            tracing::warn!(%workload, error = %why, "the pre-stop hook could not be written");
            e.error =
                korpis_proto::korpis::v1::Error { message: why.to_string(), ..Default::default() }
                    .into();
        }
        self.record(e).await;
        written.is_ok()
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
