// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn policy() -> Policy {
    Policy {
        volume_root: PathBuf::from("/var/lib/korpis/volumes"),
        shared_roots: vec![PathBuf::from("/srv/shared")],
        identity_root: PathBuf::from("/var/lib/korpis-agent/identity"),
        netns_root: PathBuf::from("/run/korpis/netns"),
        devices: crate::devices::Specs { dirs: Vec::new(), owner: 0 },
    }
}

#[test]
fn only_the_workloads_own_network_is_joined() {
    let mut own = honest();
    own.namespaces = "/run/korpis/netns/wl_1".into();
    assert!(validate(own, &policy()).is_ok());
    let other = |s: &mut driver::PreparedSpec| {
        s.namespaces = "/run/korpis/netns/wl_2".into();
    };
    assert_eq!(refused(other), "namespaces", "another workload's network");
    let host = |s: &mut driver::PreparedSpec| {
        s.namespaces = "/proc/1/ns/net".into();
    };
    assert_eq!(refused(host), "namespaces", "the host's network");
    let up = |s: &mut driver::PreparedSpec| {
        s.namespaces = "/run/korpis/netns/../netns/wl_1".into();
    };
    assert_eq!(refused(up), "namespaces", "a path that climbs");
}

fn honest() -> driver::PreparedSpec {
    driver::PreparedSpec {
        workload: "wl_1".into(),
        intent: "in_1".into(),
        rootfs: format!("registry.example/mc@sha256:{}", "a".repeat(64)),
        tier: pb::IsolationTier::ISOLATION_TIER_CONTAINER.into(),
        labels: driver::RuntimeLabels {
            workload: "wl_1".into(),
            intent: "in_1".into(),
            ..Default::default()
        }
        .into(),
        uid_base: FIRST_WORKLOAD_ID + 3 * IDS_PER_WORKLOAD,
        uid_count: IDS_PER_WORKLOAD,
        resources: pb::ResourceSpec {
            pids: pb::PidsSpec { limit: 100, ..Default::default() }.into(),
            ..Default::default()
        }
        .into(),
        mounts: vec![driver::Mount {
            source: "/var/lib/korpis/volumes/wl_1.world".into(),
            destination: "/data".into(),
            volume: "world".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn refused(f: impl FnOnce(&mut driver::PreparedSpec)) -> String {
    let mut s = honest();
    f(&mut s);
    validate(s, &policy()).expect_err("refused").field
}

#[test]
fn an_honest_spec_passes_and_every_invariant_refuses_its_breach() {
    assert!(validate(honest(), &policy()).is_ok());
    let host = |s: &mut driver::PreparedSpec| s.mounts[0].source = "/etc".into();
    assert_eq!(refused(host), "mounts.source");
    let escape = |s: &mut driver::PreparedSpec| {
        s.mounts[0].source = "/var/lib/korpis/volumes/../../../etc".into();
    };
    assert_eq!(refused(escape), "mounts.source");
    let root = |s: &mut driver::PreparedSpec| s.mounts[0].source = "/var/lib/korpis/volumes".into();
    assert_eq!(refused(root), "mounts.source", "the root itself holds every volume");
    let shared = |s: &mut driver::PreparedSpec| s.mounts[0].source = "/srv/shared/wl_1.db".into();
    let mut on_shared = honest();
    shared(&mut on_shared);
    assert!(validate(on_shared, &policy()).is_ok(), "a shared class's volume");
    let beside = |s: &mut driver::PreparedSpec| s.mounts[0].source = "/srv/sharedx/v".into();
    assert_eq!(refused(beside), "mounts.source", "not a shared root's");
    let whole = |s: &mut driver::PreparedSpec| s.mounts[0].source = "/srv/shared".into();
    assert_eq!(refused(whole), "mounts.source", "nor the shared root itself");
    assert_eq!(refused(|s| s.mounts[0].destination = "/proc".into()), "mounts.destination");
    assert_eq!(refused(|s| s.uid_base = 0), "uid_base", "remapping is not optional");
    assert_eq!(refused(|s| s.uid_base += 1), "uid_base", "a range is a whole slot");
    assert_eq!(refused(|s| s.uid_count = 1 << 20), "uid_base");
    assert_eq!(refused(|s| s.rootfs = "registry.example/mc:latest".into()), "rootfs");
    let vm = |s: &mut driver::PreparedSpec| s.tier = pb::IsolationTier::ISOLATION_TIER_VM.into();
    assert_eq!(refused(vm), "tier");
    assert_eq!(refused(|s| s.resources = pb::ResourceSpec::default().into()), "resources.pids");
    assert_eq!(
        refused(|s| {
            s.env.insert("A=B".into(), "c".into());
        }),
        "env"
    );
    assert_eq!(refused(|s| s.working_dir = "relative".into()), "working_dir");
    let port = |s: &mut driver::PreparedSpec| s.ports.push(driver::PortMapping::default());
    assert_eq!(refused(port), "ports");
    assert_eq!(refused(|s| s.labels = driver::RuntimeLabels::default().into()), "labels");
}

#[test]
fn a_stream_is_written_only_to_the_workloads_own_stream_volume() {
    let mut own = honest();
    own.stream = driver::StreamCapture {
        dir: "/var/lib/korpis/volumes/wl_1.stream_console".into(),
        ..Default::default()
    }
    .into();
    assert!(validate(own.clone(), &policy()).is_ok());
    let other = |s: &mut driver::PreparedSpec| {
        s.stream = driver::StreamCapture {
            dir: "/var/lib/korpis/volumes/wl_2.stream_console".into(),
            ..Default::default()
        }
        .into();
    };
    assert_eq!(refused(other), "stream.dir", "another workload's stream");
    let world = |s: &mut driver::PreparedSpec| {
        s.stream = driver::StreamCapture {
            dir: "/var/lib/korpis/volumes/wl_1.world".into(),
            ..Default::default()
        }
        .into();
    };
    assert_eq!(refused(world), "stream.dir", "a volume of its own that is not its stream");
}

#[test]
fn a_run_s_object_writes_its_own_run_s_stream_and_no_other() {
    let at = |dir: &str| driver::StreamCapture { dir: dir.into(), ..Default::default() };
    let mut run = honest();
    if let Some(l) = run.labels.as_option_mut() {
        l.run = "run_9".into();
    }
    run.stream = at("/var/lib/korpis/volumes/wl_1.stream_console.run_9").into();
    assert!(validate(run.clone(), &policy()).is_ok());
    let mut another = run.clone();
    another.stream = at("/var/lib/korpis/volumes/wl_1.stream_console.run_8").into();
    assert!(validate(another, &policy()).is_err(), "another run's stream");
    let mut workload_s = run;
    workload_s.stream = at("/var/lib/korpis/volumes/wl_1.stream_console").into();
    assert!(validate(workload_s, &policy()).is_err(), "the workload's, from a run's object");
}

#[test]
fn only_the_workloads_own_identity_directory_is_mounted() {
    let mut own = honest();
    own.identity_dir = "/var/lib/korpis-agent/identity/wl_1".into();
    assert!(validate(own, &policy()).is_ok());
    let other = |s: &mut driver::PreparedSpec| {
        s.identity_dir = "/var/lib/korpis-agent/identity/wl_2".into();
    };
    assert_eq!(refused(other), "identity_dir", "another workload's socket");
    let up = |s: &mut driver::PreparedSpec| {
        s.identity_dir = "/var/lib/korpis-agent/identity/wl_1/..".into();
    };
    assert_eq!(refused(up), "identity_dir");
    assert_eq!(refused(|s| s.identity_dir = "/etc".into()), "identity_dir");
    let shadow = |s: &mut driver::PreparedSpec| s.mounts[0].destination = IDENTITY_MOUNT.into();
    assert_eq!(refused(shadow), "mounts.destination", "a volume cannot cover the socket");
}
