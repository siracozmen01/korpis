// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a test sets and reads of the in-memory runtime: the calls made, a verb made to fail or to
//! wait, a rejection, an exit, consoles captured, and the terminating signal ignored.

use super::{InMemory, Object, state};
use korpis_proto::korpis::driver::v1 as driver;
use std::collections::BTreeMap;

impl InMemory {
    /// Every call so far, as `verb id`.
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        self.lock().map(|s| s.calls.clone()).unwrap_or_default()
    }

    /// Makes every later call of `verb` fail, or succeed again.
    pub fn fail(&self, verb: &str, failing: bool) {
        if let Ok(mut s) = self.lock() {
            if failing {
                s.failing.insert(verb.to_owned());
            } else {
                s.failing.remove(verb);
            }
        }
    }

    /// Makes every later call of `verb` wait until it is released, as a slow step would; a
    /// held call is recorded as `held verb` when it begins waiting.
    pub fn hold(&self, verb: &str, holding: bool) {
        if let Ok(mut s) = self.lock() {
            if holding {
                s.holding.insert(verb.to_owned());
            } else {
                s.holding.remove(verb);
            }
        }
        self.released.notify_waiters();
    }

    /// Whether `verb` is held, noting the call as held the first time it is.
    pub(super) fn holds(&self, verb: &str, noted: &mut bool) -> bool {
        let Ok(mut s) = self.lock() else { return false };
        if !s.holding.contains(verb) {
            return false;
        }
        if !*noted {
            s.calls.push(format!("held {verb}"));
            *noted = true;
        }
        true
    }

    /// Waits while `verb` is held.
    pub(super) async fn held(&self, verb: &str) {
        let mut noted = false;
        loop {
            let released = self.released.notified();
            if !self.holds(verb, &mut noted) {
                return;
            }
            released.await;
        }
    }

    /// Whether a workload whose console mode is not `none` has its console captured into a
    /// stream, as a real driver's prepare asks for: off by default, so a test that does not look
    /// at streams gets no stream volumes.
    pub fn capture_consoles(&self, capturing: bool) {
        if let Ok(mut s) = self.lock() {
            s.capturing = capturing;
        }
    }

    /// Makes every running object ignore the terminating signal, or heed it again: a
    /// container's first process with no handler for it ignores it, and only a kill stops it.
    pub fn ignore_term(&self, ignoring: bool) {
        if let Ok(mut s) = self.lock() {
            s.deaf = ignoring;
        }
    }

    /// Makes every later `prepare` reject with `rejection`, or accept again.
    pub fn reject(&self, rejection: Option<driver::Rejection>) {
        if let Ok(mut s) = self.lock() {
            s.rejecting = rejection;
        }
    }

    /// The object `id` exits by itself with `code`, as a crash would, and says so.
    pub fn exit(&self, id: &str, code: i32) {
        if let Ok(mut s) = self.lock()
            && let Some(o) = s.objects.get_mut(id)
        {
            o.status = state(driver::RuntimeState::RUNTIME_STATE_STOPPED, Some(code));
        }
        let _ = self.changes.send(id.to_owned());
    }

    /// The objects, by identifier: their workload, and whether they are running.
    #[must_use]
    pub fn objects(&self) -> BTreeMap<String, (String, bool)> {
        let Ok(s) = self.lock() else { return BTreeMap::new() };
        let running = |o: &Object| {
            o.status.state.as_known() == Some(driver::RuntimeState::RUNTIME_STATE_RUNNING)
        };
        s.objects
            .iter()
            .map(|(id, o)| (id.clone(), (o.spec.workload.clone(), running(o))))
            .collect()
    }
}
