// SPDX-License-Identifier: AGPL-3.0-or-later

//! The device verbs, kept in memory: a volume ensured with a device is attached until released.

use super::InMemoryHost;
use crate::host::{Devicing, HostError};
use korpis_proto::korpis::helper::v1 as helper;
use std::future::{Future, ready};

impl Devicing for InMemoryHost {
    fn devices(
        &self,
    ) -> impl Future<Output = Result<Vec<helper::AttachedDevice>, HostError>> + Send {
        ready(self.with(|k| {
            k.devices
                .iter()
                .map(|(volume, workload)| helper::AttachedDevice {
                    volume: volume.clone(),
                    workload: workload.clone(),
                    ..Default::default()
                })
                .collect()
        }))
    }

    fn release_device(&self, volume: &str) -> impl Future<Output = Result<bool, HostError>> + Send {
        ready(self.with(|k| k.devices.remove(volume).is_some()))
    }
}
