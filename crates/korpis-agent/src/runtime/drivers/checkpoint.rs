// SPDX-License-Identifier: AGPL-3.0-or-later

//! A checkpoint and a resume, each sent to the driver that owns the object.

use super::Drivers;
use crate::runtime::{Runtime, RuntimeResult};

impl<R: Runtime> Drivers<R> {
    pub(super) async fn checkpoint_on(&self, id: &str, into: &str) -> RuntimeResult<u64> {
        self.owner(id).await?.checkpoint(id, into).await
    }

    pub(super) async fn resume_on(&self, id: &str, from: &str) -> RuntimeResult<()> {
        self.owner(id).await?.resume(id, from).await
    }
}
