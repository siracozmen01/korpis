// SPDX-License-Identifier: AGPL-3.0-or-later

//! Devices (§6.3 of docs/05-scheduling.md): a node is kept only where its driver gives the class
//! and mode asked and enough matching devices are free beside what other placements hold, and a
//! node with devices is spared for workloads that ask for them.

use super::{Case, checks, driver, node};
use crate::node::device::{Device, DeviceHold, DeviceMode};
use crate::node::driver::DeviceClass;
use crate::schedule::{Check, Signal};
use crate::workload::spec::devices::DeviceRequest;
use uuid::Uuid;

/// A node whose driver gives GPUs in `passthrough`, with one GPU.
fn gpu_node(n: u128) -> crate::node::Node {
    let mut node = node(n, &[]);
    let mut d = driver(&["memory", "cpu"]);
    d.capabilities.devices = vec![DeviceClass::Gpu];
    d.capabilities.device_modes = vec![DeviceMode::Passthrough];
    node.drivers = vec![d];
    node.devices = vec![Device {
        id: "nvidia.com/gpu=0".into(),
        class: DeviceClass::Gpu,
        attributes: std::collections::BTreeMap::default(),
        host_nodes: vec!["/dev/nvidia0".into()],
        shares: 0,
    }];
    node
}

fn one_gpu(mode: DeviceMode) -> DeviceRequest {
    DeviceRequest { class: DeviceClass::Gpu, count: 1, selector: None, mode }
}

#[test]
fn a_workload_asking_for_a_gpu_goes_where_one_is_free_and_its_driver_gives_it() {
    let mut case = Case::new(vec![node(1, &[]), gpu_node(2), gpu_node(3)]);
    case.body.devices = vec![one_gpu(DeviceMode::Passthrough)];
    let held = DeviceHold { device: "nvidia.com/gpu=0".into(), mode: DeviceMode::Passthrough };
    case.held.insert(Uuid::from_u128(2), vec![held]);
    let e = case.run();
    assert_eq!(checks(&e, 1), [Check::Device], "no driver there gives a GPU");
    assert_eq!(checks(&e, 2), [Check::Device], "its one GPU is held");
    assert!(e.filtered.iter().any(|f| f.detail.contains("0 of 1 gpu")), "{:?}", e.filtered);
    assert_eq!(e.chosen, Some(Uuid::from_u128(3)));
    case.body.devices = vec![one_gpu(DeviceMode::Timeslice)];
    assert_eq!(checks(&case.run(), 3), [Check::Device], "its driver does not share one");
}

#[test]
fn a_node_with_devices_is_spared_for_a_workload_that_asks_for_none() {
    let e = Case::new(vec![gpu_node(1), node(2, &[])]).run();
    assert_eq!(e.chosen, Some(Uuid::from_u128(2)));
    let fit =
        |n| e.scored.iter().find(|s| s.node == Uuid::from_u128(n) && s.signal == Signal::DeviceFit);
    assert_eq!(fit(1).map(|s| s.value), Some(0.0));
    assert_eq!(fit(2).map(|s| s.value), Some(1.0));
}
