// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::labels::LabelSelector;
use crate::node::driver::DeviceClass;

/// A GPU named `id`, giving `nodes`, shared by `shares`.
fn gpu(id: &str, nodes: &[&str], shares: u32) -> Device {
    Device {
        id: format!("nvidia.com/gpu={id}"),
        class: DeviceClass::Gpu,
        attributes: [("model".to_owned(), if id.contains(':') { "mig" } else { "a100" }.into())]
            .into(),
        host_nodes: nodes.iter().map(|n| (*n).to_owned()).collect(),
        shares,
    }
}

/// GPU 0, GPU 1, and two MIG partitions of GPU 1, each naming GPU 1's node and its own.
fn machine() -> Vec<Device> {
    vec![
        gpu("0", &["/dev/nvidia0"], 0),
        gpu("1", &["/dev/nvidia1"], 0),
        gpu("1:0", &["/dev/nvidia1", "/dev/nvidia-caps/nvidia-cap10"], 2),
        gpu("1:1", &["/dev/nvidia1", "/dev/nvidia-caps/nvidia-cap20"], 0),
    ]
}

fn ask(count: u32, mode: DeviceMode) -> DeviceRequest {
    DeviceRequest { class: DeviceClass::Gpu, count, selector: None, mode }
}

fn hold(id: &str, mode: DeviceMode) -> DeviceHold {
    DeviceHold { device: format!("nvidia.com/gpu={id}"), mode }
}

fn ids(holds: &[DeviceHold]) -> Vec<&str> {
    holds.iter().map(|h| h.device.trim_start_matches("nvidia.com/gpu=")).collect()
}

#[test]
fn a_gpu_and_its_partitions_are_one_hardware_and_two_partitions_are_not() {
    let m = machine();
    assert!(same_hardware(&m[1], &m[2]), "GPU 1 and its partition");
    assert!(!same_hardware(&m[2], &m[3]), "two partitions of it");
    assert!(!same_hardware(&m[0], &m[1]), "two GPUs");
}

#[test]
fn devices_are_given_least_shared_first_and_never_held_twice() {
    let (m, pass) = (machine(), DeviceMode::Passthrough);
    assert_eq!(ids(&choose(&m, &[], &[ask(1, pass)], &[]).unwrap()), ["0"]);
    let both = choose(&m, &[hold("0", pass)], &[ask(2, pass)], &[]).unwrap();
    assert_eq!(ids(&both), ["1:0", "1:1"], "GPU 0 is held; GPU 1's partitions before itself");
    let beside = choose(&m, &[hold("1:1", pass)], &[ask(2, pass)], &[]).unwrap();
    assert_eq!(ids(&beside), ["0", "1:0"], "GPU 1 is not free beside its held partition");
    let held = [hold("0", pass), hold("1:0", pass)];
    assert_eq!(ids(&choose(&m, &held, &[ask(1, pass)], &[]).unwrap()), ["1:1"]);
    let refused = choose(&m, &[hold("0", pass), hold("1:1", pass)], &[ask(2, pass)], &[]);
    assert_eq!(refused.unwrap_err(), "1 of 2 gpu devices free in passthrough that match");
}

#[test]
fn a_shared_device_takes_its_shares_and_nothing_in_passthrough_beside_them() {
    let (m, slice) = (machine(), DeviceMode::Timeslice);
    let one = [hold("1:0", slice)];
    assert_eq!(ids(&choose(&m, &one, &[ask(1, slice)], &[]).unwrap()), ["1:0"]);
    let two = [hold("1:0", slice), hold("1:0", slice)];
    assert!(choose(&m, &two, &[ask(1, slice)], &[]).is_err(), "its two shares are taken");
    let whole = choose(&m, &one, &[ask(1, DeviceMode::Passthrough)], &[]).unwrap();
    assert_eq!(ids(&whole), ["0"], "neither the shared partition nor GPU 1 around it");
    assert!(choose(&m, &[], &[ask(1, DeviceMode::Mediated)], &[]).is_err(), "no hypervisor");
}

#[test]
fn a_selector_and_what_a_placement_holds_decide_before_the_order() {
    let (m, pass) = (machine(), DeviceMode::Passthrough);
    let mig = DeviceRequest {
        selector: Some(LabelSelector {
            match_labels: [("model".to_owned(), "mig".to_owned())].into(),
            expressions: Vec::new(),
        }),
        ..ask(1, pass)
    };
    assert_eq!(ids(&choose(&m, &[], &[mig], &[]).unwrap()), ["1:0"]);
    let kept = choose(&m, &[], &[ask(1, pass)], &[hold("1", pass)]).unwrap();
    assert_eq!(ids(&kept), ["1"], "a placement following its target keeps its GPU");
}
