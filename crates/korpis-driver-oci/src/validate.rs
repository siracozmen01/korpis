// SPDX-License-Identifier: AGPL-3.0-or-later

//! The driver's own door (§6.1 of docs/02-architecture.md). Access to containerd is root, so
//! this check, not the supervisor, is what stands between a compromised supervisor and the host:
//! every spec is checked against the container tier's invariants (§2 of docs/04-runtimes.md)
//! before anything is built from it, and a spec the supervisor could not have produced from an
//! honest assignment is refused. Only a [`Valid`] spec can be turned into a runtime spec, and only
//! this module makes one.
//!
//! What is checked here is lexical: a path is judged by its text. That a volume's source is
//! really under the helper's root, with no symbolic link leading out, is checked again on the
//! filesystem when the container is created.

use korpis_core::workload::assignment::{FIRST_WORKLOAD_ID, IDS_PER_WORKLOAD};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::path::{Component, Path, PathBuf};

/// The most processes and threads a workload may hold when its intent sets no limit: there is
/// no configuration without one (§5.2 of docs/05-scheduling.md).
pub const DEFAULT_PIDS: u64 = 4096;
/// The longest argument, environment entry, or path accepted, in bytes.
const MAX_STRING: usize = 64 << 10;
/// The most arguments, environment entries, or mounts accepted.
const MAX_ITEMS: usize = 4096;
/// Where inside a container nothing may be mounted over: the kernel's interfaces, and the root.
const RESERVED: [&str; 5] = ["/", "/proc", "/sys", "/dev", IDENTITY_MOUNT];

/// Where a workload's identity socket directory is mounted.
pub const IDENTITY_MOUNT: &str = "/run/korpis";

/// Why a spec is refused: a security event, reported as one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {why}")]
pub struct Violation {
    /// What is wrong.
    pub field: String,
    /// Why.
    pub why: String,
}

fn refuse<T>(field: &str, why: &str) -> Result<T, Violation> {
    Err(Violation { field: field.to_owned(), why: why.to_owned() })
}

/// What the driver allows, beyond the tier's invariants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// The directory the helper keeps volumes under.
    pub volume_root: PathBuf,
    /// The roots of the node's shared storage classes, each a network filesystem the operator
    /// mounted, whose volumes the helper keeps there (§2.1 of docs/06-storage.md).
    pub shared_roots: Vec<PathBuf>,
    /// The directory the helper mounts block classes' volumes under, one directory per class
    /// (§2.1 of docs/06-storage.md).
    pub block_root: PathBuf,
    /// The directory the agent makes the workloads' identity sockets under, one directory each
    /// (§9.5 of docs/08-identity.md). These two roots, the `resolv.conf` in the network
    /// directory a spec joins, and what the node's CDI files give a device, read by the driver
    /// itself, are the only host paths ever mounted.
    pub identity_root: PathBuf,
    /// The directory the helper binds workloads' network namespaces under, one each (§4.1 of
    /// docs/07-networking.md): the only namespaces a container joins rather than makes.
    pub netns_root: PathBuf,
    /// The node's Container Device Interface files, the only source of a device's edits.
    pub devices: crate::devices::Specs,
}

/// A spec that passed every check.
#[derive(Debug, Clone, PartialEq)]
pub struct Valid(driver::PreparedSpec);

impl Valid {
    /// The spec.
    #[must_use]
    pub fn spec(&self) -> &driver::PreparedSpec {
        &self.0
    }
}

/// Whether `p` is absolute and has no `.` or `..` in it.
fn clean_absolute(p: &str) -> bool {
    let path = Path::new(p);
    path.is_absolute()
        && path.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

fn text(field: &str, s: &str) -> Result<(), Violation> {
    if s.len() > MAX_STRING || s.contains('\0') {
        return refuse(field, "too long, or holds a NUL");
    }
    Ok(())
}

fn ids(spec: &driver::PreparedSpec) -> Result<(), Violation> {
    let offset = spec.uid_base.checked_sub(FIRST_WORKLOAD_ID);
    let aligned = offset.is_some_and(|o| o % IDS_PER_WORKLOAD == 0);
    let last = spec.uid_base.checked_add(IDS_PER_WORKLOAD - 1);
    if spec.uid_count != IDS_PER_WORKLOAD || !aligned || last.is_none_or(|l| l == u32::MAX) {
        return refuse(
            "uid_base",
            "not a workload id range: user-namespace remapping is not optional",
        );
    }
    Ok(())
}

fn mounts(spec: &driver::PreparedSpec, policy: &Policy) -> Result<(), Violation> {
    if spec.mounts.len() > MAX_ITEMS {
        return refuse("mounts", "too many");
    }
    for m in &spec.mounts {
        let mut roots =
            [&policy.volume_root, &policy.block_root].into_iter().chain(&policy.shared_roots);
        let under_root = roots.any(|root| {
            Path::new(&m.source)
                .strip_prefix(root)
                .is_ok_and(|rest| rest.components().next().is_some())
        });
        if !clean_absolute(&m.source) || !under_root || m.volume.is_empty() {
            return refuse("mounts.source", "only a volume the helper prepared is mounted");
        }
        if !clean_absolute(&m.destination) || RESERVED.contains(&m.destination.as_str()) {
            return refuse("mounts.destination", "a clean absolute path, not over the kernel's");
        }
        text("mounts", &m.source)?;
        text("mounts", &m.destination)?;
    }
    Ok(())
}

/// The identity directory, if any, is the one the agent made for this workload: exactly the
/// workload's directory under the identity root, so no spec can mount another's socket.
fn identity(spec: &driver::PreparedSpec, policy: &Policy) -> Result<(), Violation> {
    if spec.identity_dir.is_empty() {
        return Ok(());
    }
    let own = policy.identity_root.join(&spec.workload);
    let plain =
        spec.workload.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !plain || !clean_absolute(&spec.identity_dir) || Path::new(&spec.identity_dir) != own {
        return refuse("identity_dir", "only the workload's own identity directory is mounted");
    }
    Ok(())
}

/// The namespaces joined, if any, are the workload's own under the helper's root, so no spec
/// joins another workload's network or user namespace.
fn network(spec: &driver::PreparedSpec, policy: &Policy) -> Result<(), Violation> {
    if spec.namespaces.is_empty() {
        return Ok(());
    }
    let own = policy.netns_root.join(&spec.workload);
    if !clean_absolute(&spec.namespaces) || Path::new(&spec.namespaces) != own {
        return refuse("namespaces", "only the workload's own namespaces are joined");
    }
    Ok(())
}

/// The stream directory, if any, is the workload's own stream volume under the helper's root,
/// or, for an object that executes one of its runs, that run's (§4 of docs/14-streams.md): a
/// run writes its own console, and never another run's or the workload's.
fn stream(spec: &driver::PreparedSpec, policy: &Policy) -> Result<(), Violation> {
    let Some(s) = spec.stream.as_option() else { return Ok(()) };
    if !korpis_driver_kit::console::own_dir(spec, &policy.volume_root) {
        return refuse("stream.dir", "only the workload's own stream volume is written to");
    }
    if s.secrets.len() > MAX_ITEMS {
        return refuse("stream.secrets", "too many");
    }
    for x in &s.secrets {
        text("stream.secrets", &x.name)?;
        text("stream.secrets", &x.value)?;
    }
    Ok(())
}

fn process(spec: &driver::PreparedSpec) -> Result<(), Violation> {
    if spec.command.len() + spec.args.len() > MAX_ITEMS || spec.env.len() > MAX_ITEMS {
        return refuse("command", "too many arguments or variables");
    }
    for a in spec.command.iter().chain(&spec.args) {
        text("command", a)?;
    }
    for (k, v) in &spec.env {
        if k.is_empty() || k.contains('=') {
            return refuse("env", "a variable's name is not empty and holds no '='");
        }
        text("env", k)?;
        text("env", v)?;
    }
    if !spec.working_dir.is_empty() && !clean_absolute(&spec.working_dir) {
        return refuse("working_dir", "a clean absolute path");
    }
    Ok(())
}

fn resources(spec: &driver::PreparedSpec) -> Result<(), Violation> {
    let r = spec.resources.as_option();
    let pids = r.and_then(|r| r.pids.as_option()).map_or(0, |p| p.limit);
    if pids == 0 {
        return refuse("resources.pids", "a workload always has a process limit");
    }
    let memory = r.and_then(|r| r.memory.as_option());
    if memory.is_some_and(|m| m.limit != 0 && m.reservation > m.limit) {
        return refuse("resources.memory", "a reservation above the limit");
    }
    Ok(())
}

/// Checks `spec` against the container tier's invariants and `policy`.
pub fn validate(spec: driver::PreparedSpec, policy: &Policy) -> Result<Valid, Violation> {
    let tier = spec.tier.as_known();
    if !matches!(tier, Some(pb::IsolationTier::ISOLATION_TIER_CONTAINER)) {
        return refuse("tier", "this driver runs the container tier only");
    }
    let rootfs = spec.rootfs.split_once('@').map(|(_, d)| d);
    if rootfs.is_none_or(|d| korpis_core::content::hex_of(d).is_err()) {
        return refuse("rootfs", "an image is named by its digest");
    }
    let labels = spec.labels.as_option();
    if labels.is_none_or(|l| l.workload.is_empty() || l.intent.is_empty())
        || spec.workload.is_empty()
    {
        return refuse("labels", "every runtime object is labelled for adoption");
    }
    if !spec.ports.is_empty() {
        return refuse("ports", "the node forwards a workload's ports, so a spec names none");
    }
    ids(&spec)?;
    mounts(&spec, policy)?;
    identity(&spec, policy)?;
    network(&spec, policy)?;
    stream(&spec, policy)?;
    process(&spec)?;
    resources(&spec)?;
    crate::devices::lexical(&spec)?;
    Ok(Valid(spec))
}

#[cfg(test)]
mod tests;
