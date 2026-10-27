// SPDX-License-Identifier: AGPL-3.0-or-later

//! The filter (§2.1 of docs/05-scheduling.md): which nodes can run this, each refusal with its
//! reason. Every check is evaluated, so a node rejected for two reasons shows both.

use super::topology::{Fleet, domain, matching};
use super::{Candidate, Check, Peer, PlacementPolicy, RESERVED, Request};
use crate::node::driver::{DriverInfo, StorageFeature, class_named};
use crate::node::{NodeAdmin, NodeStatus, Reachability};
use crate::tenancy::quota::Held;
use crate::workload::spec::ConsoleMode;
use crate::workload::spec::endpoints::Exposure;
use crate::workload::spec::health::ProbeKind;
use crate::workload::spec::placement::AffinityRule;
use crate::workload::spec::resources::CpuIsolation;
use crate::workload::spec::volumes::VolumeSpec;

type Refusal = (Check, String);

fn owner(req: &Request<'_>, c: &Candidate<'_>) -> Option<Refusal> {
    (!req.owners.contains(&c.node.organization)).then(|| {
        (Check::Owner, "enrolled into an organization this workload is not below".to_owned())
    })
}

fn status(req: &Request<'_>, c: &Candidate<'_>) -> Option<Refusal> {
    let here = req.current.is_some_and(|cur| cur.node == c.node.id);
    // Cordoning stops new placements; a workload already here may change in place.
    let ok = if here {
        c.node.admin != NodeAdmin::Decommissioned
    } else {
        c.node.status() == NodeStatus::Ready
    };
    (!ok).then(|| (Check::Status, c.node.status().as_str().to_owned()))
}

/// Whether a probe of the intent is of a kind `is`.
fn probes(req: &Request<'_>, is: impl Fn(&ProbeKind) -> bool) -> bool {
    let health = &req.body.health;
    let mut probes = [&health.readiness, &health.liveness].into_iter().flatten();
    probes.any(|p| is(&p.kind))
}

/// Why `d` cannot run the intent's probes: an `exec` probe runs a command inside the workload,
/// which only a driver that declares `exec` can do; a `heartbeat` probe hears from the workload
/// through its identity socket, which only a driver that gives it its directory serves; and a
/// `guest_agent` probe asks a guest's own agent, which only a driver that declares `guest_agent`
/// reaches (§5 of docs/04-runtimes.md).
fn unprobed(req: &Request<'_>, d: &DriverInfo) -> Option<String> {
    if !d.capabilities.exec && probes(req, |k| matches!(k, ProbeKind::Exec { .. })) {
        return Some(format!(
            "{:?} runs no command inside a workload, as its exec probe needs",
            d.name
        ));
    }
    let heartbeat = |k: &ProbeKind| matches!(k, ProbeKind::Heartbeat { .. });
    if !d.capabilities.identity_dir && probes(req, heartbeat) {
        let why = "gives a workload no identity socket, as its heartbeat probe needs";
        return Some(format!("{:?} {why}", d.name));
    }
    let agent = |k: &ProbeKind| matches!(k, ProbeKind::GuestAgent);
    (!d.capabilities.guest_agent && probes(req, agent)).then(|| {
        format!("{:?} reaches no agent inside a guest, as its guest_agent probe needs", d.name)
    })
}

/// The driver checks: name, tier, console, probes, and the limits it holds. The first failing
/// one is the reason, because each depends on the one before it having a driver to ask.
fn driver<'n>(req: &Request<'_>, c: &Candidate<'n>) -> Result<&'n DriverInfo, Refusal> {
    let rt = &req.body.runtime;
    let Some(d) = c.node.drivers.iter().find(|d| d.name == rt.driver && d.healthy) else {
        return Err((Check::Driver, format!("no healthy {:?} driver", rt.driver)));
    };
    if !d.capabilities.tiers.contains(&rt.tier) {
        return Err((Check::Tier, format!("{:?} does not offer the {:?} tier", d.name, rt.tier)));
    }
    let console = req.body.console;
    if console != ConsoleMode::None && !d.capabilities.console.contains(&console) {
        return Err((Check::Console, format!("{:?} offers no {console:?} console", d.name)));
    }
    if let Some(why) = unprobed(req, d) {
        return Err((Check::Probe, why));
    }
    Ok(d)
}

fn shaped(s: &crate::workload::spec::endpoints::ShapingSpec) -> bool {
    s.egress_bits_per_sec > 0 || s.ingress_bits_per_sec > 0
}

/// The kinds of limit the intent sets, by the names drivers declare enforcement under: an
/// endpoint's own shaping and its accounting among them, which a node's network holds (§7 of
/// docs/07-networking.md).
fn limited(req: &Request<'_>) -> Vec<&'static str> {
    let r = &req.body.resources;
    let io = r.io;
    let cpu = r.cpu.limit_millis.max(r.cpu.reservation_millis);
    [
        ("memory", r.memory.effective() > 0),
        ("cpu", cpu > 0),
        // Whole cores pinned, held by a cpuset (§5.2 of docs/05-scheduling.md).
        ("dedicated_cores", r.cpu.isolation == CpuIsolation::DedicatedCores),
        ("disk", r.disk.effective() > 0),
        (
            "io",
            io.read_bytes_per_sec > 0
                || io.write_bytes_per_sec > 0
                || io.read_ops_per_sec > 0
                || io.write_ops_per_sec > 0,
        ),
        ("pids", r.pids_limit > 0),
        ("egress", r.bandwidth.egress.effective() > 0),
        ("ingress", r.bandwidth.ingress_limit > 0),
        ("endpoint_shaping", req.body.endpoints.iter().any(|e| shaped(&e.shaping))),
        ("endpoint_accounting", req.body.endpoints.iter().any(|e| e.accounting)),
    ]
    .into_iter()
    .filter_map(|(k, set)| set.then_some(k))
    .collect()
}

/// The devices the workload asks for, of a class and mode the driver gives, free on the node
/// beside what its other placements hold (§6.3 of docs/05-scheduling.md).
fn devices(req: &Request<'_>, c: &Candidate<'_>, d: &DriverInfo) -> Option<Refusal> {
    if req.body.devices.is_empty() {
        return None;
    }
    if let Some(r) = req.body.devices.iter().find(|r| !d.capabilities.gives(r.class, r.mode)) {
        let why = format!("{:?} gives no {} device in {:?}", d.name, r.class.as_str(), r.mode);
        return Some((Check::Device, why));
    }
    super::devices::given(req, c.node).err().map(|why| (Check::Device, why))
}

/// The cores the workload asks for, offered on the node and free beside what its other
/// placements hold (§5.2 of docs/05-scheduling.md).
fn cores(req: &Request<'_>, c: &Candidate<'_>) -> Option<Refusal> {
    super::cores::given(req, c.node).err().map(|why| (Check::Cores, why))
}

/// Rule K-3: a limit the kernel does not hold is not accepted, so a node whose driver cannot
/// hold one the intent sets cannot run it.
fn enforcement(req: &Request<'_>, d: &DriverInfo) -> Option<Refusal> {
    let missing: Vec<_> =
        limited(req).into_iter().filter(|k| !d.capabilities.enforces(k)).collect();
    (!missing.is_empty()).then(|| {
        let what = missing.join(", ");
        (Check::Enforcement, format!("{:?} does not enforce {what} limits", d.name))
    })
}

/// The exposures that need the node itself to be reachable (§9 of docs/07-networking.md),
/// judged on what each declared exposure resolves to (§3 there): `stable` without an edge tier
/// is `node_bound`. `ingress` an edge group does not serve is served by the node's own proxy, on
/// ports 80 and 443 it holds (§3.2 there), whatever serves the workload's `stable` endpoints
/// (Finding 197 of docs/23-walkthroughs.md). `stable` with an edge tier and `ingress` a group
/// serves reach the node through the mesh, and `overlay_only` needs the overlay only for a
/// consumer on another node, which is a cross-node dependency.
fn reachability(req: &Request<'_>, c: &Candidate<'_>) -> Option<Refusal> {
    let public = c.node.reachability == Reachability::Public;
    let serves_ingress = c.node.advertised.as_ref().is_some_and(|a| a.ingress);
    req.body.endpoints.iter().find_map(|e| match e.exposure.resolved(req.edge_tier) {
        Exposure::NodePort | Exposure::NodeBound if !public => {
            let without = if e.exposure == Exposure::Stable { " without an edge tier" } else { "" };
            Some((Check::Reachability, format!("{:?} needs a public address{without}", e.name)))
        }
        Exposure::Ingress if !req.ingress_edge && !serves_ingress => Some((
            Check::Reachability,
            format!("{:?} needs a node serving the ingress, holding ports 80 and 443", e.name),
        )),
        _ => None,
    })
}

fn selector(req: &Request<'_>, c: &Candidate<'_>) -> Option<Refusal> {
    let sel = &req.body.placement.node_selector;
    (!sel.matches(c.node.labels.as_map()))
        .then(|| (Check::Selector, "does not match the node selector".to_owned()))
}

fn taints(req: &Request<'_>, c: &Candidate<'_>) -> Option<Refusal> {
    let tolerations = &req.body.placement.tolerations;
    c.node
        .taints
        .iter()
        .find(|t| {
            !tolerations
                .iter()
                .any(|o| o.key == t.key && (o.value.is_empty() || o.value == t.value))
        })
        .map(|t| (Check::Taint, format!("{}={} is not tolerated", t.key, t.value)))
}

fn capacity(req: &Request<'_>, c: &Candidate<'_>) -> Vec<Refusal> {
    // Dedicated cores reserve what they take here; too few free is the cores check's to say.
    let held = super::cores::given(req, c.node).unwrap_or_default();
    let need = super::cores::reservation_on((req.body, req.edge_tier), c.node, &held);
    let own = req.current.filter(|cur| cur.node == c.node.id).map(|cur| cur.reserved);
    RESERVED
        .into_iter()
        .filter_map(|d| {
            let wanted = need.get(&d).copied().unwrap_or(0);
            let allocatable = c.node.allocatable.get(&d).copied().unwrap_or(0);
            let allocated = c.allocated.get(&d).copied().unwrap_or(0);
            let mine = own.and_then(|o| o.get(&d)).copied().unwrap_or(0);
            let free = allocatable.saturating_sub(allocated.saturating_sub(mine));
            (wanted > free)
                .then(|| (Check::Capacity, format!("{}: needs {wanted}, {free} free", d.as_str())))
        })
        .collect()
}

/// An existing volume the node does not reach: on another node's disk, or in a store the node
/// offers no class of.
fn volumes(req: &Request<'_>, c: &Candidate<'_>) -> Option<Refusal> {
    let classes = &c.node.storage_classes;
    let (volume, home) = req.volumes.iter().find(|(_, h)| !h.reached_by(c.node.id, classes))?;
    Some((Check::Volume, format!("volume {volume} is {home}")))
}

fn storage_classes(req: &Request<'_>, c: &Candidate<'_>) -> Option<Refusal> {
    let classes = &c.node.storage_classes;
    let missing = req
        .body
        .volumes
        .iter()
        .filter(|v| !v.storage_class.is_empty())
        .find(|v| !classes.iter().any(|s| s.name == v.storage_class))
        .map(|v| (Check::StorageClass, format!("no storage class {:?}", v.storage_class)));
    // A volume with replicas is sent to them as increments, which its class must be able to
    // send (§2.2 of docs/06-storage.md).
    let sends = |v: &&VolumeSpec| {
        class_named(classes, &v.storage_class)
            .is_some_and(|k| k.capabilities.has(StorageFeature::IncrementalSend))
    };
    missing.or_else(|| {
        let unable =
            req.body.volumes.iter().filter(|v| v.replication.is_some()).find(|v| !sends(v));
        unable.map(|v| {
            let why =
                format!("volume {:?} has replicas, and its class sends no increments", v.name);
            (Check::StorageClass, why)
        })
    })
}

/// One required rule against one candidate.
fn affinity_rule(
    req: &Request<'_>,
    c: &Candidate<'_>,
    fleet: &Fleet<'_>,
    peers: &[Peer],
    rule: &AffinityRule,
) -> Option<Refusal> {
    let key = &rule.topology_key;
    let Some(here) = domain(c.node, key) else {
        return Some((Check::Affinity, format!("no {key:?} label")));
    };
    let matched = matching(fleet, peers, key, &rule.workloads, req.workload);
    let together = matched.iter().any(|(_, d)| d.as_deref() == Some(here.as_str()));
    if rule.anti {
        return together.then(|| (Check::Affinity, format!("a workload it avoids is in {here}")));
    }
    // With nothing to be together with yet, a workload that matches its own rule is the
    // first of its group and lands anywhere; otherwise it waits for what it needs.
    let first = matched.is_empty() && rule.workloads.matches(req.labels);
    (!together && !first).then(|| (Check::Affinity, format!("nothing it needs is in {here}")))
}

fn affinity(
    req: &Request<'_>,
    c: &Candidate<'_>,
    fleet: &Fleet<'_>,
    peers: &[Peer],
    policy: &PlacementPolicy,
) -> Vec<Refusal> {
    let rules = req.body.placement.affinity.iter().chain(&policy.affinity);
    rules.filter(|r| r.required).filter_map(|r| affinity_rule(req, c, fleet, peers, r)).collect()
}

/// A dependency's placed targets must each be on the candidate or on a node the candidate
/// reaches through the mesh (§9.1 of docs/07-networking.md); a required one with none placed
/// leaves the workload nowhere to start.
fn dependencies(req: &Request<'_>, c: &Candidate<'_>, peers: &[Peer]) -> Vec<Refusal> {
    let mut out = Vec::new();
    for d in req.dependencies {
        let mut nodes = peers.iter().filter(|p| d.targets.contains(&p.workload)).map(|p| p.node);
        let name = &d.name;
        let placed = nodes.clone().next().is_some();
        let why = if !placed && d.required {
            Some(format!("its required dependency {name} is not placed"))
        } else if nodes.any(|n| n != c.node.id && !req.mesh.joins(c.node.id, n)) {
            Some(format!(
                "its dependency {name} is on a node this one does not reach through the mesh"
            ))
        } else {
            None
        };
        out.extend(why.map(|w| (Check::Dependency, w)));
    }
    out
}

/// A workload with a `dedicated_ip` endpoint holds one address, which a node may hold only where
/// a pool of that node's organization reaches it (§3.7 of docs/07-networking.md); so does one
/// whose `stable` endpoints are routed, at a `cluster` pool's address (§3.1 there).
fn dedicated(req: &Request<'_>, c: &Candidate<'_>) -> Option<Refusal> {
    let asked =
        req.routed || req.body.endpoints.iter().any(|e| e.exposure == Exposure::DedicatedIp);
    (asked && !req.dedicated.contains(&c.node.id)).then(|| {
        let why = "no address pool serving it reaches this node with an address it may hold";
        (Check::DedicatedIp, why.to_owned())
    })
}

/// A workload whose `stable` endpoints an edge group serves is reached through a GRE link from
/// each member to its node inside the mesh, and one whose `ingress` endpoints a group serves by
/// each member's proxy through the mesh, so the node reaches every member, or is one (§3.1 of
/// docs/07-networking.md).
fn edge(req: &Request<'_>, c: &Candidate<'_>) -> Option<Refusal> {
    let stable =
        req.body.endpoints.iter().any(|e| e.exposure.resolved(req.edge_tier) == Exposure::Stable);
    let node = c.node.id;
    let apart = req.edge_members.iter().find(|m| **m != node && !req.mesh.joins(node, **m));
    apart.filter(|_| stable || req.ingress_edge).map(|m| {
        (Check::Edge, format!("does not reach edge {m} of the group serving it through the mesh"))
    })
}

/// Every quota entry counting what runs on the candidate that what this placement would run
/// there exceeds (§5.1 of docs/05-scheduling.md, "Where a workload runs"). Only growth is
/// refused, as at declaration: what an entry already counts of the workload is its before. The
/// refusal is told without its numbers, since whoever may read the node, its owning tenant
/// among them, may not read the set's owner (Finding 140 of docs/23-walkthroughs.md).
fn quota(req: &Request<'_>, c: &Candidate<'_>) -> Vec<Refusal> {
    let held = Held::of(req.body);
    req.quota
        .iter()
        .filter(|q| q.entry.counts_on(c.node.labels.as_map()))
        .filter_map(|q| q.entry.admit_promised(&q.rest, &q.promises, q.counted, &held).err())
        .map(|e| (Check::Quota, e.unnumbered()))
        .collect()
}

/// Every check the candidate fails; empty if it can run the workload.
pub(super) fn rejections(
    req: &Request<'_>,
    c: &Candidate<'_>,
    fleet: &Fleet<'_>,
    peers: &[Peer],
    policy: &PlacementPolicy,
) -> Vec<Refusal> {
    let mut out: Vec<Refusal> = [owner(req, c), status(req, c)].into_iter().flatten().collect();
    match driver(req, c) {
        Ok(d) => {
            out.extend(enforcement(req, d));
            out.extend(devices(req, c, d));
        }
        Err(r) => out.push(r),
    }
    out.extend([reachability(req, c), selector(req, c), taints(req, c)].into_iter().flatten());
    out.extend(cores(req, c));
    out.extend(capacity(req, c));
    out.extend(storage_classes(req, c));
    out.extend(volumes(req, c));
    out.extend(affinity(req, c, fleet, peers, policy));
    out.extend(dependencies(req, c, peers));
    out.extend(dedicated(req, c));
    out.extend(edge(req, c));
    out.extend(quota(req, c));
    if let Some(why) = req.refused.get(&c.node.id) {
        out.push((Check::Prepare, format!("its driver refused it: {why}")));
    }
    out
}
