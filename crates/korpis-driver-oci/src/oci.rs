// SPDX-License-Identifier: AGPL-3.0-or-later

//! The OCI runtime specification of a container, built from a [`Valid`] spec and the image's
//! configuration: the container tier's hardening, always and never as an option (§2 of
//! docs/04-runtimes.md). User-namespace remapping onto the workload's id range, a read-only root
//! filesystem, `no-new-privileges`, a short capability list, the kernel's sensitive interfaces
//! masked, the seccomp allowlist, every namespace of its own (its network namespace holds only
//! loopback until the networking of Phase 1), and its limits as cgroup v2 enforces them. A volume
//! is bind-mounted with an idmapped mount, so its files keep workload-relative ids on disk (§4.5).

use crate::image::ImageConfig;
use crate::seccomp::{Seccomp, profile};
use crate::validate::Valid;
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
    access: &'static str,
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

/// The command: the spec's if it names one, else the image's entrypoint, with the spec's
/// arguments if it has any and the image's otherwise.
fn argv(valid: &Valid, image: &ImageConfig) -> Vec<String> {
    let s = valid.spec();
    if !s.command.is_empty() {
        return s.command.iter().chain(&s.args).cloned().collect();
    }
    let args = if s.args.is_empty() { &image.cmd } else { &s.args };
    image.entrypoint.iter().chain(args).cloned().collect()
}

/// The image's environment, with the spec's variables replacing any of the same name.
fn environment(valid: &Valid, image: &ImageConfig) -> Vec<String> {
    let spec = &valid.spec().env;
    let own = |e: &&String| e.split_once('=').is_none_or(|(k, _)| !spec.contains_key(k));
    image
        .env
        .iter()
        .filter(own)
        .cloned()
        .chain(spec.iter().map(|(k, v)| format!("{k}={v}")))
        .collect()
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
        devices: vec![DeviceRule { allow: false, access: "rwm" }],
        memory,
        cpu,
        pids: Pids { limit: signed(pids) },
    }
}

/// The runtime specification of the container `valid` describes, running `image`, in the
/// cgroup at `cgroups_path`.
pub fn runtime_spec(
    valid: &Valid,
    image: &ImageConfig,
    cgroups_path: &str,
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
    let caps: Vec<&'static str> = CAPABILITIES.to_vec();
    let cwd =
        [s.working_dir.as_str(), image.working_dir.as_str()].into_iter().find(|w| !w.is_empty());
    let kinds = ["pid", "ipc", "uts", "mount", "network", "user", "cgroup"];
    Ok(RuntimeSpec {
        oci_version: OCI_VERSION,
        process: Process {
            terminal: false,
            user: User { uid: image.user.0, gid: image.user.1 },
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
        linux: Linux {
            uid_mappings: map.clone(),
            gid_mappings: map,
            namespaces: kinds.into_iter().map(|kind| Namespace { kind }).collect(),
            resources: resources(valid),
            cgroups_path: cgroups_path.to_owned(),
            masked_paths: MASKED.to_vec(),
            readonly_paths: READ_ONLY.to_vec(),
            seccomp: profile(),
        },
    })
}

#[cfg(test)]
mod tests;
