// SPDX-License-Identifier: AGPL-3.0-or-later

//! The device verbs of block classes (§2.1 of docs/06-storage.md).

use super::{HelperClient, failed};
use crate::host::HostError;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;

impl HelperClient {
    /// Every device a block class has attached here.
    pub async fn devices(&self) -> Result<Vec<helper::AttachedDevice>, HostError> {
        match self.call(Verb::ListDevices(Box::default())).await? {
            Answer::ListDevices(a) => Ok(a.devices),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }

    /// Unmounts the volume `volume` and lets go of its device; whether one was attached for it.
    pub async fn release_device(&self, volume: &str) -> Result<bool, HostError> {
        let r = helper::ReleaseDeviceRequest { volume: volume.to_owned(), ..Default::default() };
        match self.call(Verb::ReleaseDevice(Box::new(r))).await? {
            Answer::ReleaseDevice(a) => Ok(a.released),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }
}
