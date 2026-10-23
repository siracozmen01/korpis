// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a spec makes of a guest: its shape (vCPUs, memory, and the cgroup its Firecracker runs
//! in), and the config drive `korpis-guest` reads (§6 of docs/04-runtimes.md). Each limit is held
//! by the hypervisor or the kernel: the guest's memory is its size, its CPU its vCPUs and its
//! Firecracker's `cpu.max`, and the Firecracker process is bounded beyond the guest's memory only
//! by what it needs itself.

use crate::jail::Shape;
use korpis_core::content::image::ImageConfig;
use korpis_core::workload::network::{GATEWAY_V4, GATEWAY_V6, RESOLVER_V4};
use korpis_driver_kit::entry;
use korpis_guest::config::{Config, Network, Volume};
use korpis_proto::korpis::driver::v1 as driver;
use std::collections::BTreeMap;
use std::net::IpAddr;

const MIB: u64 = 1 << 20;

/// A guest's memory when its spec sets none.
pub const DEFAULT_MEMORY_MIB: u64 = 128;

/// The least memory a guest is given: below it the kernel does not boot.
pub const MIN_MEMORY_MIB: u64 = 64;

/// What its Firecracker may use beyond the guest's memory: the VMM, its threads, and the page
/// cache of its drives' files.
pub const OVERHEAD_MIB: u64 = 64;

/// The most vCPUs Firecracker gives a guest.
pub const MAX_VCPUS: u64 = 32;

/// The period `cpu.max` divides time into, in microseconds.
const CPU_PERIOD: u64 = 100_000;

/// The shape `spec` asks for.
#[must_use]
pub fn shape(spec: &driver::PreparedSpec) -> Shape {
    let r = spec.resources.as_option();
    let memory = r.and_then(|r| r.memory.as_option());
    let bytes = memory.map_or(0, |m| if m.limit > 0 { m.limit } else { m.reservation });
    let memory_mib = if bytes == 0 { DEFAULT_MEMORY_MIB } else { bytes.div_ceil(MIB) };
    let memory_mib = memory_mib.max(MIN_MEMORY_MIB);
    let cpu = r.and_then(|r| r.cpu.as_option());
    let millis =
        cpu.map_or(0, |c| if c.limit_millis > 0 { c.limit_millis } else { c.reservation_millis });
    let vcpus = millis.div_ceil(1000).clamp(1, MAX_VCPUS);
    let mut cgroup = vec![
        format!("memory.max={}", (memory_mib + OVERHEAD_MIB) * MIB),
        format!("pids.max={}", 64 + vcpus),
    ];
    if let Some(limit) = cpu.map(|c| c.limit_millis).filter(|l| *l > 0) {
        cgroup.push(format!("cpu.max={} {CPU_PERIOD}", limit * (CPU_PERIOD / 1000)));
    }
    let netns = Some(spec.namespaces.as_str())
        .filter(|n| !n.is_empty())
        .map(|n| std::path::Path::new(n).join("net"));
    Shape { vcpus: u32::try_from(vcpus).unwrap_or(1), memory_mib, cgroup, netns }
}

/// The guest's network, from the addresses `spec` gives it: each on its `eth0`, a default route
/// through the gateway of each family it has an address of, and the workload's resolver, as a
/// container is given them (§4.1 and §5.1 of docs/07-networking.md); none without addresses.
pub fn network(spec: &driver::PreparedSpec) -> Result<Option<Network>, String> {
    if spec.addresses.is_empty() {
        return Ok(None);
    }
    let mut addresses = Vec::new();
    for a in &spec.addresses {
        let bad = || format!("{a:?} is not an address with its prefix");
        let (ip, len) = a.split_once('/').ok_or_else(bad)?;
        let ip: IpAddr = ip.parse().map_err(|_| bad())?;
        let len: u8 = len.parse().map_err(|_| bad())?;
        let whole = if ip.is_ipv4() { 32 } else { 128 };
        let twice = addresses.iter().any(|(o, _): &(IpAddr, u8)| o.is_ipv4() == ip.is_ipv4());
        if len != whole || twice {
            return Err(format!("{a:?} is not the one host address of its family"));
        }
        addresses.push((ip, len));
    }
    let mut gateways = vec![IpAddr::V4(GATEWAY_V4)];
    if addresses.iter().any(|(ip, _)| ip.is_ipv6()) {
        gateways.push(IpAddr::V6(GATEWAY_V6));
    }
    if !addresses.iter().any(|(ip, _)| ip.is_ipv4()) {
        return Err("a guest's addresses include one of IPv4".into());
    }
    Ok(Some(Network { addresses, gateways, resolvers: vec![IpAddr::V4(RESOLVER_V4)] }))
}

/// The config drive for `spec`, whose image's configuration is `image`.
pub fn config(spec: &driver::PreparedSpec, image: &ImageConfig) -> Result<Config, String> {
    let argv = entry::argv(spec, image);
    if argv.is_empty() {
        return Err("the image names no command, and neither does the recipe".into());
    }
    let env: BTreeMap<String, String> = entry::environment(spec, image)
        .into_iter()
        .filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
        .collect();
    let volumes = spec
        .mounts
        .iter()
        .map(|m| Volume { path: m.destination.clone(), read_only: m.read_only })
        .collect();
    Ok(Config {
        hostname: spec.workload.clone(),
        argv,
        env,
        working_dir: entry::working_dir(spec, image),
        uid: image.user.0,
        gid: image.user.1,
        network: network(spec)?,
        volumes,
        interactive: spec.stream.as_option().is_some_and(|s| s.input),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_proto::korpis::v1 as pb;

    fn spec(memory: u64, cpu: u64) -> driver::PreparedSpec {
        let resources = pb::ResourceSpec {
            memory: pb::ReservedLimit { limit: memory, ..Default::default() }.into(),
            cpu: pb::CpuSpec { limit_millis: cpu, ..Default::default() }.into(),
            ..Default::default()
        };
        driver::PreparedSpec { resources: resources.into(), ..Default::default() }
    }

    #[test]
    fn a_guest_is_as_large_as_its_limits_and_its_firecracker_a_little_larger() {
        let s = shape(&spec(512 * MIB, 1500));
        assert_eq!((s.vcpus, s.memory_mib), (2, 512));
        assert!(s.cgroup.contains(&format!("memory.max={}", 576 * MIB)));
        assert!(s.cgroup.contains(&"cpu.max=150000 100000".to_owned()));
        let small = shape(&spec(0, 0));
        assert_eq!((small.vcpus, small.memory_mib), (1, DEFAULT_MEMORY_MIB));
        assert!(!small.cgroup.iter().any(|c| c.starts_with("cpu.max")), "no limit, no quota");
        assert_eq!(shape(&spec(MIB, 0)).memory_mib, MIN_MEMORY_MIB);
    }

    #[test]
    fn the_config_runs_the_image_as_its_user_with_the_spec_s_environment() {
        let mut s = spec(0, 0);
        s.workload = "wl_1".into();
        s.env.insert("GREETING".into(), "hi".into());
        s.mounts.push(driver::Mount { destination: "/data".into(), ..Default::default() });
        let image = ImageConfig {
            user: (1000, 100),
            env: vec!["PATH=/bin".into(), "GREETING=old".into()],
            cmd: vec!["sh".into()],
            ..ImageConfig::default()
        };
        let c = config(&s, &image).unwrap();
        assert_eq!((c.uid, c.gid, c.argv.clone()), (1000, 100, vec!["sh".to_owned()]));
        assert_eq!(c.env.get("GREETING").map(String::as_str), Some("hi"));
        assert_eq!((c.working_dir.as_str(), c.volumes.len()), ("/", 1));
        assert!(config(&s, &ImageConfig::default()).is_err(), "nothing to run");
    }
}
