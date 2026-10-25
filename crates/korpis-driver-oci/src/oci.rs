// SPDX-License-Identifier: AGPL-3.0-or-later

//! The OCI runtime specification of a container, built from a [`Valid`] spec and the image's
//! configuration: the container tier's hardening, always and never as an option (§2 of
//! docs/04-runtimes.md). User-namespace remapping onto the workload's id range, a read-only root
//! filesystem, `no-new-privileges`, a short capability list, the kernel's sensitive interfaces
//! masked, the seccomp allowlist, every namespace of its own (its network namespace holds only
//! loopback until the networking of Phase 1), and its limits as cgroup v2 enforces them. A volume
//! is bind-mounted with an idmapped mount, so its files keep workload-relative ids on disk (§4.5).

use crate::devices::Given;
use crate::seccomp::{Seccomp, profile};
use crate::validate::{IDENTITY_MOUNT, Valid};
use korpis_core::content::image::ImageConfig;
use serde::Serialize;

/// The OCI runtime specification version written.
const OCI_VERSION: &str = "1.2.0";
/// The capabilities a container keeps, all of them namespaced to its user namespace: enough to
/// switch to its own users, own its own files, and bind a low port, and nothing that reaches
/// the kernel's administration (no `SYS_ADMIN`, `NET_ADMIN`, `NET_RAW`, `MKNOD`, `SYS_PTRACE`).
const CAPABILITIES: [&str; 9] = [
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_FOWNER",
    "CAP_FSETID",
    "CAP_KILL",
    "CAP_NET_BIND_SERVICE",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETPCAP",
];
const MASKED: [&str; 10] = [
    "/proc/acpi",
    "/proc/asound",
    "/proc/kcore",
    "/proc/keys",
    "/proc/latency_stats",
    "/proc/timer_list",
    "/proc/timer_stats",
    "/proc/sched_debug",
    "/proc/scsi",
    "/sys/firmware",
];
const READ_ONLY: [&str; 5] =
    ["/proc/bus", "/proc/fs", "/proc/irq", "/proc/sys", "/proc/sysrq-trigger"];
/// A cgroup v2 CPU period, in microseconds.
const CPU_PERIOD: u64 = 100_000;

/// Why no runtime spec can be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BuildError(pub String);

/// One id mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdMapping {
    container_id: u32,
    host_id: u32,
    size: u32,
}

/// A mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mount {
    destination: String,
    #[serde(rename = "type")]
    kind: String,
    source: String,
    options: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    uid_mappings: Vec<IdMapping>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    gid_mappings: Vec<IdMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct User {
    uid: u32,
    gid: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    additional_gids: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Capabilities {
    bounding: Vec<&'static str>,
    effective: Vec<&'static str>,
    permitted: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Rlimit {
    #[serde(rename = "type")]
    kind: &'static str,
    hard: u64,
    soft: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Process {
    terminal: bool,
    user: User,
    args: Vec<String>,
    env: Vec<String>,
    cwd: String,
    capabilities: Capabilities,
    rlimits: Vec<Rlimit>,
    no_new_privileges: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Root {
    path: &'static str,
    readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Namespace {
    #[serde(rename = "type")]
    kind: &'static str,
    /// An existing namespace to join instead of making one.
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

/// The namespaces a container runs in: each its own, except where the agent had a network made
/// for it (§4.1 of docs/07-networking.md), whose network namespace and the user namespace that
/// owns it, in the directory `made`, it joins by path. Its capabilities leave out
/// `CAP_NET_ADMIN`, so it cannot change the network it joins.
fn namespaces(made: &str) -> Vec<Namespace> {
    let kinds = ["pid", "ipc", "uts", "mount", "network", "user", "cgroup"];
    let joined = |file: &str| (!made.is_empty()).then(|| format!("{made}/{file}"));
    kinds
        .into_iter()
        .map(|kind| match kind {
            "network" => Namespace { kind, path: joined("net") },
            "user" => Namespace { kind, path: joined("user") },
            _ => Namespace { kind, path: None },
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct Memory {
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reservation: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    swap: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct Cpu {
    #[serde(skip_serializing_if = "Option::is_none")]
    cpus: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mems: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shares: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quota: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    period: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Pids {
    limit: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DeviceRule {
    allow: bool,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    major: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    minor: Option<i64>,
    access: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Resources {
    devices: Vec<DeviceRule>,
    memory: Memory,
    cpu: Cpu,
    pids: Pids,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Linux {
    uid_mappings: Vec<IdMapping>,
    gid_mappings: Vec<IdMapping>,
    namespaces: Vec<Namespace>,
    resources: Resources,
    cgroups_path: String,
    masked_paths: Vec<&'static str>,
    readonly_paths: Vec<&'static str>,
    seccomp: Seccomp,
}

/// A container's OCI runtime specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSpec {
    oci_version: &'static str,
    process: Process,
    root: Root,
    hostname: String,
    mounts: Vec<Mount>,
    #[serde(skip_serializing_if = "devices::Hooks::is_empty")]
    hooks: devices::Hooks,
    linux: Linux,
}

fn mount(destination: &str, kind: &str, source: &str, options: &[&str]) -> Mount {
    Mount {
        destination: destination.to_owned(),
        kind: kind.to_owned(),
        source: source.to_owned(),
        options: options.iter().map(|o| (*o).to_owned()).collect(),
        uid_mappings: Vec::new(),
        gid_mappings: Vec::new(),
    }
}

/// The kernel's file systems, and writable scratch space, since the root is read-only.
fn system_mounts() -> Vec<Mount> {
    let scratch = ["nosuid", "nodev", "mode=1777", "size=65536k"];
    vec![
        mount("/proc", "proc", "proc", &["nosuid", "noexec", "nodev"]),
        mount("/dev", "tmpfs", "tmpfs", &["nosuid", "strictatime", "mode=755", "size=65536k"]),
        mount(
            "/dev/pts",
            "devpts",
            "devpts",
            &["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"],
        ),
        mount(
            "/dev/shm",
            "tmpfs",
            "shm",
            &["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"],
        ),
        mount("/dev/mqueue", "mqueue", "mqueue", &["nosuid", "noexec", "nodev"]),
        mount("/sys", "sysfs", "sysfs", &["nosuid", "noexec", "nodev", "ro"]),
        mount(
            "/sys/fs/cgroup",
            "cgroup",
            "cgroup",
            &["nosuid", "noexec", "nodev", "relatime", "ro"],
        ),
        mount("/tmp", "tmpfs", "tmpfs", &scratch),
        mount("/run", "tmpfs", "tmpfs", &scratch),
    ]
}

/// The command, as every driver starts an image (`korpis_driver_kit::entry`).
fn argv(valid: &Valid, image: &ImageConfig) -> Vec<String> {
    korpis_driver_kit::entry::argv(valid.spec(), image)
}

/// The environment, as every driver gives an image its own (`korpis_driver_kit::entry`).
fn environment(valid: &Valid, image: &ImageConfig) -> Vec<String> {
    korpis_driver_kit::entry::environment(valid.spec(), image)
}

fn signed(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn resources(valid: &Valid) -> Resources {
    let r = valid.spec().resources.as_option();
    let mut memory = Memory::default();
    if let Some(m) = r.and_then(|r| r.memory.as_option()) {
        memory.limit = (m.limit > 0).then(|| signed(m.limit));
        memory.swap = memory.limit;
        memory.reservation = (m.reservation > 0).then(|| signed(m.reservation));
    }
    let mut cpu = Cpu::default();
    if let Some(c) = r.and_then(|r| r.cpu.as_option()) {
        if c.limit_millis > 0 {
            cpu.quota = Some(signed(c.limit_millis.saturating_mul(CPU_PERIOD / 1000)));
            cpu.period = Some(CPU_PERIOD);
        }
        cpu.shares =
            (c.reservation_millis > 0).then(|| (c.reservation_millis * 1024 / 1000).max(2));
    }
    let pids = r.and_then(|r| r.pids.as_option()).map_or(0, |p| p.limit);
    Resources {
        devices: vec![DeviceRule {
            allow: false,
            kind: None,
            major: None,
            minor: None,
            access: "rwm".into(),
        }],
        memory,
        cpu,
        pids: Pids { limit: signed(pids) },
    }
}

/// The runtime specification of the container `valid` describes, running `image`, in the
/// cgroup at `cgroups_path`, given what its devices give.
pub fn runtime_spec(
    valid: &Valid,
    image: &ImageConfig,
    cgroups_path: &str,
    given: &Given,
) -> Result<RuntimeSpec, BuildError> {
    let s = valid.spec();
    let args = argv(valid, image);
    if args.is_empty() {
        return Err(BuildError("neither the recipe nor the image names a command".into()));
    }
    let map = vec![IdMapping { container_id: 0, host_id: s.uid_base, size: s.uid_count }];
    let mut mounts = system_mounts();
    for m in &s.mounts {
        let mut options = vec!["rbind", "nosuid", "nodev"];
        options.push(if m.read_only { "ro" } else { "rw" });
        let mut volume = mount(&m.destination, "bind", &m.source, &options);
        volume.uid_mappings.clone_from(&map);
        volume.gid_mappings.clone_from(&map);
        mounts.push(volume);
    }
    // The identity socket's directory: read-only, and not id-mapped, so the socket keeps the
    // agent's ownership and the workload connects through its mode (§9.5 of
    // docs/08-identity.md).
    if !s.identity_dir.is_empty() {
        let options = ["rbind", "nosuid", "nodev", "noexec", "ro"];
        mounts.push(mount(IDENTITY_MOUNT, "bind", &s.identity_dir, &options));
    }
    // The resolver its network was made with, named alone (§5.1 of docs/07-networking.md):
    // read-only, so the workload cannot point itself elsewhere through the file.
    let resolv = std::path::Path::new(&s.namespaces).join("resolv.conf");
    if !s.namespaces.is_empty() && resolv.is_file() {
        let options = ["rbind", "nosuid", "nodev", "noexec", "ro"];
        mounts.push(mount("/etc/resolv.conf", "bind", &resolv.to_string_lossy(), &options));
    }
    let caps: Vec<&'static str> = CAPABILITIES.to_vec();
    let cwd =
        [s.working_dir.as_str(), image.working_dir.as_str()].into_iter().find(|w| !w.is_empty());
    let mut spec = RuntimeSpec {
        oci_version: OCI_VERSION,
        process: Process {
            // An interactive console runs under a terminal (§6 of docs/14-streams.md).
            terminal: s.stream.as_option().is_some_and(|c| c.input),
            user: User { uid: image.user.0, gid: image.user.1, additional_gids: Vec::new() },
            args,
            env: environment(valid, image),
            cwd: cwd.unwrap_or("/").to_owned(),
            capabilities: Capabilities {
                bounding: caps.clone(),
                effective: caps.clone(),
                permitted: caps,
            },
            rlimits: vec![Rlimit { kind: "RLIMIT_NOFILE", hard: 1 << 20, soft: 1 << 20 }],
            no_new_privileges: true,
        },
        root: Root { path: "rootfs", readonly: true },
        hostname: s.workload.clone(),
        mounts,
        hooks: devices::Hooks::default(),
        linux: Linux {
            // Given also when the user namespace is joined, which already maps the range: runc
            // then refuses a namespace whose mapping is not this one.
            uid_mappings: map.clone(),
            gid_mappings: map,
            namespaces: namespaces(&s.namespaces),
            resources: resources(valid),
            cgroups_path: cgroups_path.to_owned(),
            masked_paths: MASKED.to_vec(),
            readonly_paths: READ_ONLY.to_vec(),
            seccomp: profile(),
        },
    };
    devices::apply(&mut spec, given);
    Ok(spec)
}

mod cores;
mod devices;
#[cfg(test)]
mod tests;
