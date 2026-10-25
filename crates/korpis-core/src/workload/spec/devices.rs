// SPDX-License-Identifier: AGPL-3.0-or-later

//! What devices a workload holds while it is placed (§6.3 of docs/05-scheduling.md): each request
//! a class, a count, a selector over the devices' attributes, and a mode.

use super::SpecError;
use crate::labels::LabelSelector;
use crate::node::device::DeviceMode;
use crate::node::driver::DeviceClass;

/// The most requests a workload makes, and the most devices one asks for: a machine holds a few
/// accelerators, and the bound keeps a placement's search small.
const MOST_REQUESTS: usize = 8;
const MOST_COUNT: u32 = 16;

/// A request for devices of one class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRequest {
    /// The class.
    pub class: DeviceClass,
    /// How many; at least one.
    pub count: u32,
    /// Over the devices' attributes and `id`; any device of the class when `None`.
    pub selector: Option<LabelSelector>,
    /// How each is held.
    pub mode: DeviceMode,
}

/// Checks `requests`: at most eight, each for one to sixteen devices.
///
/// # Errors
/// The first request that breaks a bound, named by its field.
pub fn validate(requests: &[DeviceRequest]) -> Result<(), SpecError> {
    if requests.len() > MOST_REQUESTS {
        return Err(SpecError::new("devices", "at most 8 requests"));
    }
    if let Some(i) = requests.iter().position(|r| r.count == 0 || r.count > MOST_COUNT) {
        return Err(SpecError::new(&format!("devices[{i}].count"), "from 1 to 16"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpus(count: u32) -> DeviceRequest {
        DeviceRequest {
            class: DeviceClass::Gpu,
            count,
            selector: None,
            mode: DeviceMode::Passthrough,
        }
    }

    #[test]
    fn a_request_asks_for_one_to_sixteen_and_a_workload_makes_at_most_eight() {
        assert!(validate(&[gpus(1), gpus(16)]).is_ok());
        assert_eq!(validate(&[gpus(0)]).unwrap_err().field, "devices[0].count");
        assert_eq!(validate(&[gpus(1), gpus(17)]).unwrap_err().field, "devices[1].count");
        assert_eq!(validate(&vec![gpus(1); 9]).unwrap_err().field, "devices");
    }
}
