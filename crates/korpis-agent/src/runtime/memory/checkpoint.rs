// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the in-memory runtime does as a driver taking snapshots would: a checkpoint leaves a record
//! in the directory it is given and stops the object, and a resume starts it only from a directory
//! holding one.

use super::{InMemory, state};
use crate::runtime::{RuntimeError, RuntimeResult};
use korpis_proto::korpis::driver::v1 as driver;

/// The file this runtime's checkpoint leaves in the directory it is given.
const CHECKPOINT: &str = "state.json";

impl InMemory {
    /// A checkpoint, as this runtime keeps one: a record of where, in the directory `into`, and
    /// the object stopped; resumed only from a directory holding one.
    pub(super) fn checkpoint_now(&self, id: &str, into: &str) -> RuntimeResult<u64> {
        std::fs::write(std::path::Path::new(into).join(CHECKPOINT), id)
            .map_err(|e| RuntimeError::Failed(format!("the checkpoint: {e}")))?;
        self.set("checkpoint", id, state(driver::RuntimeState::RUNTIME_STATE_STOPPED, Some(0)))?;
        Ok(id.len() as u64)
    }

    pub(super) fn resume_now(&self, id: &str, from: &str) -> RuntimeResult<()> {
        if !std::path::Path::new(from).join(CHECKPOINT).is_file() {
            return Err(RuntimeError::Failed(format!("refused: no state in {from}")));
        }
        self.set("resume", id, state(driver::RuntimeState::RUNTIME_STATE_RUNNING, None))
    }

}
