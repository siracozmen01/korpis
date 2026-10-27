// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the agent asks of the helper for block classes' devices (§2.1 of docs/06-storage.md):
//! every device attached here, with the workload it was attached for, and letting go of one.

use super::HostError;
use super::node::NodeHost;
use korpis_proto::korpis::helper::v1 as helper;
use std::future::Future;

/// The helper's device verbs.
pub trait Devicing: Send + Sync + 'static {
    /// Every device a block class has attached here.
    fn devices(
        &self,
    ) -> impl Future<Output = Result<Vec<helper::AttachedDevice>, HostError>> + Send;

    /// Unmounts the volume `volume` and lets go of its device, which the storage keeps; whether
    /// one was attached for it.
    fn release_device(&self, volume: &str) -> impl Future<Output = Result<bool, HostError>> + Send;
}

impl Devicing for NodeHost {
    fn devices(
        &self,
    ) -> impl Future<Output = Result<Vec<helper::AttachedDevice>, HostError>> + Send {
        self.helper().devices()
    }

    fn release_device(&self, volume: &str) -> impl Future<Output = Result<bool, HostError>> + Send {
        self.helper().release_device(volume)
    }
}
