// SPDX-License-Identifier: AGPL-3.0-or-later

//! A volume's passes (§8.1 of docs/05-scheduling.md). The control plane starts each pass, naming
//! it and the pass it is an increment from, and judges it from both nodes' reports: the bytes the
//! source sent, and the destination's word that it holds the pass. In `replicate` a pass held
//! that was small ends the volume's part, and one that was not starts the next; in `final_sync`
//! the one pass held ends it, the pass started as the phase is entered. A pass whose ticket ran
//! out before the destination held it is started again, under a new name, from the same parent.

use super::{Crossing, MigrationVolume, Phase, small};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// A pass to start for a volume: its name, and the pass it is an increment from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewPass {
    /// The volume.
    pub volume: Uuid,
    /// The pass.
    pub pass: Uuid,
    /// The pass it is an increment from; `None` for a whole one, and always for a tree, which
    /// sends what differs.
    pub parent: Option<Uuid>,
}

impl MigrationVolume {
    /// Whether the destination holds the pass under way.
    fn held(&self) -> bool {
        self.pass.is_some_and(|(p, _)| self.held_pass == Some(p))
    }

    /// Whether its part of `phase` is done: nothing is sent for it, or, in `replicate`, the
    /// pass under way is held and was small, or, in `final_sync`, it is held.
    #[must_use]
    pub fn passed(&self, phase: Phase) -> bool {
        if !self.crossing.sends() {
            return true;
        }
        match phase {
            Phase::Replicate => {
                self.held()
                    && self.sent_bytes.is_some_and(|s| small(s, self.size_bytes, self.passes))
            }
            Phase::FinalSync => self.held(),
            _ => true,
        }
    }

    /// The pass to start for it in `phase` at `now`, named `name`, if one is due within the
    /// phase: the first where none is under way, the next in `replicate` once the one under way
    /// is held and was large, or the one under way again once its ticket, lasting `lifetime`,
    /// ran out before the destination held it.
    #[must_use]
    pub fn pass_due(
        &self,
        phase: Phase,
        (now, lifetime): (SystemTime, Duration),
        name: Uuid,
    ) -> Option<NewPass> {
        if !self.crossing.sends() || !matches!(phase, Phase::Replicate | Phase::FinalSync) {
            return None;
        }
        let Some((_, since)) = self.pass else { return self.entry_pass(name) };
        if self.held() {
            let large = phase == Phase::Replicate && self.sent_bytes.is_some();
            return (large && !self.passed(phase)).then(|| self.entry_pass(name)).flatten();
        }
        let expired = since + lifetime <= now;
        expired.then_some(NewPass { volume: self.volume, pass: name, parent: self.parent_pass })
    }

    /// The pass that starts a phase sending it, named `name`: an increment from the last pass
    /// the destination held, where it is sent by increments; `None` where nothing is sent.
    #[must_use]
    pub fn entry_pass(&self, name: Uuid) -> Option<NewPass> {
        let parent = if self.crossing == Crossing::Increments { self.held_pass } else { None };
        self.crossing.sends().then_some(NewPass { volume: self.volume, pass: name, parent })
    }

    /// Starts `p` on it.
    pub fn start(&mut self, p: NewPass, at: SystemTime) {
        self.passes += 1;
        self.pass = Some((p.pass, at));
        self.parent_pass = p.parent;
        self.sent_bytes = None;
    }
}
