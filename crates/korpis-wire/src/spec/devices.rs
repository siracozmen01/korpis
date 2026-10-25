// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's device requests (§6.3 of docs/05-scheduling.md): a mode left unspecified is
//! `passthrough`, and a selector left empty selects any device of the class.

use crate::node::{device_from_pb, device_to_pb, mode_from_pb, mode_to_pb};
use crate::scope::{selector_from_pb, selector_to_pb};
use crate::{Result, invalid};
use korpis_core::node::device::DeviceMode;
use korpis_core::workload::spec::devices::DeviceRequest;
use korpis_proto::korpis::v1 as pb;

/// The wire form of `requests`.
#[must_use]
pub fn requests_to_pb(requests: &[DeviceRequest]) -> Vec<pb::DeviceRequest> {
    requests
        .iter()
        .map(|r| pb::DeviceRequest {
            class: device_to_pb(r.class).into(),
            count: r.count,
            selector: r.selector.as_ref().map(selector_to_pb).into(),
            mode: mode_to_pb(r.mode).into(),
            ..Default::default()
        })
        .collect()
}

/// The requests in the field `field`.
///
/// # Errors
/// A request names no class, an unknown mode, or a selector that does not parse.
pub fn requests_from_pb(field: &str, v: &[pb::DeviceRequest]) -> Result<Vec<DeviceRequest>> {
    v.iter()
        .enumerate()
        .map(|(i, r)| {
            let at = |name: &str| format!("{field}[{i}].{name}");
            let mode = if r.mode.as_known() == Some(pb::DeviceMode::DEVICE_MODE_UNSPECIFIED) {
                DeviceMode::Passthrough
            } else {
                mode_from_pb(&at("mode"), r.mode)?
            };
            let selector = selector_from_pb(&at("selector"), r.selector.as_option())
                .map_err(|e| invalid(&at("selector"), e.to_string()))?;
            Ok(DeviceRequest {
                class: device_from_pb(&at("class"), r.class)?,
                count: r.count,
                selector: (selector != korpis_core::labels::LabelSelector::default())
                    .then_some(selector),
                mode,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_core::node::driver::DeviceClass;

    #[test]
    fn a_request_reads_back_and_an_unset_mode_is_passthrough() {
        let asked = DeviceRequest {
            class: DeviceClass::Gpu,
            count: 2,
            selector: None,
            mode: DeviceMode::Timeslice,
        };
        assert_eq!(
            requests_from_pb("d", &requests_to_pb(std::slice::from_ref(&asked))).unwrap(),
            [asked]
        );
        let bare = pb::DeviceRequest {
            class: pb::DeviceClass::DEVICE_CLASS_USB.into(),
            count: 1,
            ..Default::default()
        };
        let read = requests_from_pb("d", &[bare]).unwrap();
        assert_eq!((read[0].mode, read[0].selector.is_none()), (DeviceMode::Passthrough, true));
        let classless = pb::DeviceRequest { count: 1, ..Default::default() };
        assert!(requests_from_pb("d", &[classless]).is_err());
    }
}
