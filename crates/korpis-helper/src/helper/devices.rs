// SPDX-License-Identifier: AGPL-3.0-or-later

//! The device verbs of block classes (§2.1 of docs/06-storage.md): letting go of a volume's
//! device, which the storage keeps, and listing every device attached here with the workload it
//! was attached for, which the agent lets go of once nothing holds it.

use super::Helper;
use crate::error::{HelperError, HelperResult};
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;

impl Helper {
    /// Answers a device verb.
    pub(super) fn device_verb(&mut self, verb: Verb) -> HelperResult<Answer> {
        Ok(match verb {
            Verb::ReleaseDevice(r) => {
                let released = match self.holding(&r.volume) {
                    Ok(c) => c.release_device(&r.volume)?,
                    Err(HelperError::NotFound(_)) => self.release_unmounted(&r.volume)?,
                    Err(e) => return Err(e),
                };
                let answer = helper::ReleaseDeviceResponse { released, ..Default::default() };
                Answer::ReleaseDevice(Box::new(answer))
            }
            Verb::ListDevices(_) => Answer::ListDevices(Box::new(self.devices()?)),
            _ => return Err(HelperError::Refused("not a device verb".into())),
        })
    }

    /// Lets go of a device attached for `volume` whose volume directory is gone already, as one
    /// left by a mount that failed; whether one was.
    fn release_unmounted(&mut self, volume: &str) -> HelperResult<bool> {
        for class in &self.classes {
            if class.devices.as_ref().is_some() && class.release_device(volume)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every device the block classes have attached here.
    fn devices(&self) -> HelperResult<helper::ListDevicesResponse> {
        let mut devices = Vec::new();
        for class in &self.classes {
            let Some(d) = class.devices.as_ref() else { continue };
            for (volume, workload) in d.attached()? {
                devices.push(helper::AttachedDevice {
                    volume,
                    workload,
                    storage_class: class.name().to_owned(),
                    ..Default::default()
                });
            }
        }
        Ok(helper::ListDevicesResponse { devices, ..Default::default() })
    }
}
