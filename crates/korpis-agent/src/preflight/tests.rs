// SPDX-License-Identifier: AGPL-3.0-or-later

//! Preflight against machines that are not this one: a copy of `/proc`, `/sys`, and `/dev` in a
//! directory, written as each case needs.

use super::*;
use korpis_proto::korpis::v1 as pb;

/// A machine whose files are `files`, beneath a fresh directory.
fn machine(files: &[(&str, &str)]) -> (tempfile::TempDir, Machine) {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let at = dir.path().join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, content).unwrap();
    }
    let m = Machine {
        proc: dir.path().join("proc"),
        sys: dir.path().join("sys"),
        dev: dir.path().join("dev"),
    };
    (dir, m)
}

/// A machine with everything a node needs.
const GOOD: &[(&str, &str)] = &[
    ("proc/sys/kernel/osrelease", "6.8.0-142-generic\n"),
    ("sys/fs/cgroup/cgroup.controllers", "cpuset cpu io memory hugetlb pids rdma misc\n"),
    ("proc/sys/user/max_user_namespaces", "63000\n"),
    ("proc/sys/kernel/seccomp/actions_avail", "kill_process kill_thread trap errno\n"),
    ("proc/sys/net/ipv4/ip_forward", "1\n"),
    ("proc/sys/net/ipv6/conf/all/disable_ipv6", "0\n"),
    ("sys/fs/cgroup/io.cost.qos", "259:0 enable=1 ctrl=auto rpct=0.00\n"),
    ("proc/cpuinfo", "processor : 0\nflags : fpu vme sse2\n"),
    ("dev/kvm", ""),
    ("sys/devices/system/cpu/online", "0-3\n"),
    ("sys/devices/system/cpu/cpu0/topology/core_cpus_list", "0-1\n"),
    ("sys/devices/system/cpu/cpu1/topology/core_cpus_list", "0-1\n"),
    ("sys/devices/system/cpu/cpu2/topology/core_cpus_list", "2-3\n"),
    ("sys/devices/system/cpu/cpu3/topology/core_cpus_list", "2-3\n"),
    ("sys/fs/cgroup/cgroup.subtree_control", "cpuset cpu io memory pids\n"),
    ("sys/fs/cgroup/korpis/cpuset.cpus.exclusive", "\n"),
    ("sys/fs/cgroup/korpis/cpuset.cpus.partition", "member\n"),
    ("proc/irq/default_smp_affinity", "3\n"),
];

fn class(name: &str, backend: pb::StorageBackend) -> pb::StorageClassInfo {
    pb::StorageClassInfo { name: name.into(), backend: backend.into(), ..Default::default() }
}

fn probes() -> Probes {
    Probes {
        landlock_abi: Some(5),
        nft: Some(("nftables v1.0.6 (Lester Gooch #5)".into(), true)),
        containerd: Some(Ok(())),
        tiers: Some(Ok(vec![pb::IsolationTier::ISOLATION_TIER_CONTAINER])),
        storage: Some(Ok(vec![class("local", pb::StorageBackend::STORAGE_BACKEND_XFS_PROJECT)])),
        volume_disk: Some(("259:4".into(), "259:0".into())),
        shaping: Some(Ok(())),
    }
}

fn oci() -> Offer {
    Offer { driver: "oci".into(), dedicated_cores: None }
}

fn outcome<'a>(r: &'a Report, name: &str) -> &'a Outcome {
    &r.0.iter().find(|c| c.name == name).unwrap().outcome
}

#[test]
fn a_machine_with_everything_offers_everything_and_says_kvm_is_not_needed_without_microvms() {
    let (_dir, m) = machine(GOOD);
    let r = checks(&m, &probes(), &oci());
    for c in &r.0 {
        assert!(
            matches!(c.outcome, Outcome::Met | Outcome::NotNeeded(_)),
            "{}: {:?}",
            c.name,
            c.outcome
        );
    }
    assert!(matches!(outcome(&r, "KVM"), Outcome::NotNeeded(_)));
    assert!(r.offers_anything());
    assert!(r.text("This node").contains("Nothing on this machine was changed."));
}

#[test]
fn an_old_kernel_or_no_cgroup_v2_leaves_the_node_nothing_to_offer() {
    let mut files = GOOD.to_vec();
    files[0] = ("proc/sys/kernel/osrelease", "5.15.0-91-generic\n");
    let (_dir, m) = machine(&files);
    let r = checks(&m, &probes(), &oci());
    assert!(matches!(outcome(&r, "Linux 6.1+"), Outcome::Lacking { .. }));
    assert!(!r.offers_anything());
    assert!(r.text("This node").contains("[NO] Linux 6.1+"));

    let files: Vec<_> = GOOD.iter().copied().filter(|(p, _)| !p.starts_with("sys/fs")).collect();
    let (_dir, m) = machine(&files);
    let r = checks(&m, &probes(), &oci());
    let Outcome::Lacking { fix, .. } = outcome(&r, "cgroup v2") else { panic!() };
    assert!(fix.contains("systemd.unified_cgroup_hierarchy=1"));
    assert!(!r.offers_anything());
}

#[test]
fn a_missing_controller_is_named_and_what_it_costs_said() {
    let mut files = GOOD.to_vec();
    files[1] = ("sys/fs/cgroup/cgroup.controllers", "cpu memory pids\n");
    let (_dir, m) = machine(&files);
    let r = checks(&m, &probes(), &oci());
    let Outcome::Lacking { costs, .. } = outcome(&r, "cgroup v2") else { panic!() };
    assert_eq!(costs, "the io limits cannot be enforced");
}

#[test]
fn what_a_node_can_do_without_is_lacking_and_never_fatal() {
    let mut files: Vec<_> = GOOD.iter().copied().filter(|(p, _)| !p.contains("io.cost")).collect();
    files.push(("proc/sys/net/ipv4/ip_forward", "0\n"));
    files.push(("sys/kernel/security/lsm", "lockdown,capability,yama,apparmor"));
    let (_dir, m) = machine(&files);
    let mut p = probes();
    p.landlock_abi = None;
    p.nft = Some(("nftables v0.9.3 (Topsy)".into(), false));
    p.storage = Some(Ok(vec![class("local", pb::StorageBackend::STORAGE_BACKEND_DIRECTORY)]));
    let r = checks(&m, &p, &oci());
    let Outcome::Lacking { fix, .. } = outcome(&r, "Landlock") else { panic!() };
    assert_eq!(
        fix,
        "add landlock to the lsm= kernel parameter, e.g. \
         lsm=landlock,lockdown,capability,yama,apparmor, and reboot"
    );
    let Outcome::Lacking { fix, .. } = outcome(&r, "io.cost") else { panic!() };
    assert_eq!(fix, "echo \"259:0 enable=1\" > /sys/fs/cgroup/io.cost.qos, at every boot");
    for name in ["nftables 1.0+, JSON", "IP forwarding", "enforced storage"] {
        assert!(matches!(outcome(&r, name), Outcome::Lacking { .. }), "{name}");
    }
    assert!(r.offers_anything(), "it offers less, and still runs");
}

#[test]
fn nested_virtualization_is_looked_at_only_inside_a_virtual_machine() {
    let (_dir, m) = machine(GOOD);
    let r = checks(&m, &probes(), &oci());
    let kvm = r.0.iter().find(|c| c.name == "KVM").unwrap();
    assert!(!kvm.found.contains("nested"), "{}", kvm.found);

    let mut files = GOOD.to_vec();
    files[7] = ("proc/cpuinfo", "flags : fpu vme hypervisor\n");
    files.push(("sys/module/kvm_intel/parameters/nested", "N\n"));
    let (_dir, m) = machine(&files);
    let r = checks(&m, &probes(), &oci());
    let kvm = r.0.iter().find(|c| c.name == "KVM").unwrap();
    assert!(kvm.found.ends_with("a virtual machine, nested virtualization off"), "{}", kvm.found);
}

#[test]
fn what_could_not_be_asked_is_unchecked_and_says_how_to_ask_it() {
    let (_dir, m) = machine(GOOD);
    let mut p = probes();
    p.storage = Some(Err("connection refused".into()));
    p.containerd = None;
    let r = checks(&m, &p, &Offer { driver: "memory".into(), dedicated_cores: None });
    let Outcome::Unchecked(why) = outcome(&r, "enforced storage") else { panic!() };
    assert!(why.contains("systemctl start korpis-helper.socket"), "{why}");
    assert!(matches!(outcome(&r, "containerd"), Outcome::NotNeeded(_)));
    let json = r.json();
    assert_eq!(json["checks"][0]["status"], "met");
    assert_eq!(json["offers_anything"], true);
}

#[test]
fn kvm_and_the_tun_driver_are_needed_where_a_driver_offers_microvms_and_lacking_never_fatal() {
    let microvm = || {
        let tiers = vec![
            pb::IsolationTier::ISOLATION_TIER_CONTAINER,
            pb::IsolationTier::ISOLATION_TIER_MICROVM,
        ];
        Probes { tiers: Some(Ok(tiers)), ..probes() }
    };
    let mut good = GOOD.to_vec();
    good.push(("dev/net/tun", ""));
    let (_dir, m) = machine(&good);
    let r = checks(&m, &microvm(), &oci());
    assert_eq!((outcome(&r, "KVM"), outcome(&r, "tun")), (&Outcome::Met, &Outcome::Met));
    let bare: Vec<_> = GOOD.iter().copied().filter(|(p, _)| *p != "dev/kvm").collect();
    let (_dir, m) = machine(&bare);
    let r = checks(&m, &microvm(), &oci());
    assert!(matches!(outcome(&r, "KVM"), Outcome::Lacking { .. }));
    assert!(matches!(outcome(&r, "tun"), Outcome::Lacking { .. }));
    assert!(r.offers_anything(), "the node still offers containers");
    let unasked =
        Probes { tiers: Some(Err("the driver at /run/x.sock: refused".into())), ..probes() };
    assert!(matches!(outcome(&checks(&m, &unasked, &oci()), "tun"), Outcome::Unchecked(_)));
}

#[test]
fn dedicated_cores_need_a_core_besides_cpu_0s_partitions_and_interrupts_steered_off_them() {
    let with = |changed: &[(&'static str, &'static str)]| {
        let mut files: Vec<_> =
            GOOD.iter().copied().filter(|(p, _)| changed.iter().all(|(c, _)| c != p)).collect();
        files.extend(changed.iter().copied().filter(|(_, content)| !content.is_empty()));
        machine(&files)
    };
    let (_dir, m) = with(&[("proc/irq/default_smp_affinity", "f\n")]);
    let r = checks(&m, &probes(), &oci());
    let Outcome::Lacking { costs, fix } = outcome(&r, "interrupt affinity") else { panic!() };
    assert!(costs.contains("CPUs 2-3"), "{costs}");
    assert!(fix.starts_with("boot with irqaffinity=0-1 "), "{fix}");

    let (_dir, m) = with(&[("sys/fs/cgroup/cgroup.subtree_control", "cpu io memory\n")]);
    let r = checks(&m, &probes(), &oci());
    let Outcome::Lacking { fix, .. } = outcome(&r, "cpuset partitions") else { panic!() };
    assert!(fix.starts_with("echo +cpuset >") && fix.contains("--dedicated-cores-host shared"));
    let (_dir, m) = with(&[("sys/fs/cgroup/korpis/cpuset.cpus.exclusive", "")]);
    let r = checks(&m, &probes(), &oci());
    let Outcome::Lacking { fix, .. } = outcome(&r, "cpuset partitions") else { panic!() };
    assert!(fix.starts_with("Linux 6.7 or later"), "{fix}");
    assert!(r.offers_anything(), "a node without them still offers the rest");

    let named = Offer { dedicated_cores: Some([2, 4].into_iter().collect()), ..oci() };
    let (_dir, m) = machine(GOOD);
    let r = checks(&m, &probes(), &named);
    let Outcome::Lacking { costs, .. } = outcome(&r, "physical cores") else { panic!() };
    assert_eq!(costs, "cores 4 are not offered");

    let (_dir, m) = with(&[
        ("sys/devices/system/cpu/online", "0-1\n"),
        ("sys/devices/system/cpu/cpu2/topology/core_cpus_list", ""),
        ("sys/devices/system/cpu/cpu3/topology/core_cpus_list", ""),
    ]);
    let r = checks(&m, &probes(), &oci());
    assert!(matches!(outcome(&r, "physical cores"), Outcome::Lacking { .. }));
    assert!(matches!(outcome(&r, "cpuset partitions"), Outcome::NotNeeded(_)));
    assert!(matches!(outcome(&r, "interrupt affinity"), Outcome::NotNeeded(_)));
}
