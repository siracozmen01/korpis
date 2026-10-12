// SPDX-License-Identifier: AGPL-3.0-or-later

//! The parts of a declaration the recipe can complete.

use crate::recipe::runtime::{PresetEndpoint, PresetResources, PresetVolume, RecipeRuntime};
use crate::value::ValueMap;
use crate::workload::spec::endpoints::{
    EndpointSpec, Exposure, NetworkPolicy, Protocol, ShapingSpec, TlsSpec,
};
use crate::workload::spec::resources::ResourceSpec;
use crate::workload::spec::volumes::{VolumeMode, VolumeSpec};
use crate::workload::spec::{IsolationTier, RuntimeSpec, SpecError};
use uuid::Uuid;

/// The driver a tier runs on when the declaration names none: the Linux driver of §3 of
/// docs/04-runtimes.md for that tier. A workload for another operating system names its driver.
#[must_use]
pub fn default_driver(tier: IsolationTier) -> &'static str {
    match tier {
        IsolationTier::Process => "native",
        IsolationTier::Container => "oci",
        IsolationTier::MicroVm => "firecracker",
        IsolationTier::Vm => "qemu",
    }
}

/// A runtime as declared.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeclaredRuntime {
    /// The driver; the tier's default when absent.
    pub driver: Option<String>,
    /// The tier; the recipe's first target's when absent.
    pub tier: Option<IsolationTier>,
    /// Driver-specific configuration.
    pub config: ValueMap,
}

impl DeclaredRuntime {
    pub(super) fn complete(self, recipe: &RecipeRuntime) -> Result<RuntimeSpec, SpecError> {
        let tier = match self.tier {
            Some(t) => t,
            None => recipe
                .targets
                .first()
                .map(|t| t.tier)
                .ok_or_else(|| SpecError::new("runtime.tier", "the recipe has no target"))?,
        };
        if !recipe.targets.iter().any(|t| t.tier == tier) {
            return Err(SpecError::new("runtime.tier", "the recipe has no target at this tier"));
        }
        let driver = self.driver.unwrap_or_else(|| default_driver(tier).to_owned());
        Ok(RuntimeSpec { driver, tier, config: self.config })
    }
}

/// A volume as declared: the preset's volume of the same name supplies what is left out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeclaredVolume {
    /// Its name.
    pub name: String,
    /// The mount path; the preset's when absent.
    pub path: Option<String>,
    /// The size; the preset's default when absent.
    pub size_bytes: Option<u64>,
    /// The storage class; empty is the node's default.
    pub storage_class: String,
    /// How it is mounted.
    pub mode: VolumeMode,
    /// An existing volume to attach instead of creating one.
    pub existing: Option<Uuid>,
}

/// An endpoint as declared: the preset's endpoint of the same name supplies what is left out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeclaredEndpoint {
    /// Its name.
    pub name: String,
    /// The protocol; the preset's when absent.
    pub protocol: Option<Protocol>,
    /// The port inside the workload; the preset's when absent.
    pub container_port: Option<u16>,
    /// How the network reaches it; the preset's default when absent.
    pub exposure: Option<Exposure>,
    /// Domains routed to it.
    pub domains: Vec<String>,
    /// How TLS is terminated.
    pub tls: Option<TlsSpec>,
    /// Its network policy.
    pub policy: NetworkPolicy,
    /// Whether traffic is metered.
    pub accounting: bool,
    /// Its shaping.
    pub shaping: ShapingSpec,
}

/// Resources: when nothing is declared, the preset's recommendation is reserved; whatever is
/// declared must reach the preset's minimum, which is what the software needs to start.
pub(super) fn resources(
    mut r: ResourceSpec,
    p: PresetResources,
) -> Result<ResourceSpec, SpecError> {
    let pick = |rec: u64, min: u64| if rec > 0 { rec } else { min };
    if r.memory.reservation == 0 && r.memory.limit == 0 {
        r.memory.reservation = pick(p.memory_recommended_bytes, p.memory_min_bytes);
    }
    if r.cpu.reservation_millis == 0 && r.cpu.limit_millis == 0 {
        r.cpu.reservation_millis = pick(p.cpu_recommended_millis, p.cpu_min_millis);
    }
    if r.memory.effective() < p.memory_min_bytes {
        return Err(SpecError::new("resources.memory", "below the recipe's minimum"));
    }
    let cpu = if r.cpu.limit_millis > 0 { r.cpu.limit_millis } else { r.cpu.reservation_millis };
    if cpu < p.cpu_min_millis {
        return Err(SpecError::new("resources.cpu", "below the recipe's minimum"));
    }
    Ok(r)
}

/// Volumes: every preset volume, completed by a declared one of the same name, then every
/// declared volume the preset does not name.
pub(super) fn volumes(
    declared: Vec<DeclaredVolume>,
    preset: &[PresetVolume],
) -> Result<Vec<VolumeSpec>, SpecError> {
    let mut out: Vec<VolumeSpec> = preset
        .iter()
        .map(|p| VolumeSpec {
            name: p.name.clone(),
            path: p.path.clone(),
            size_bytes: p.size_default_bytes,
            storage_class: String::new(),
            mode: VolumeMode::ReadWrite,
            existing: None,
        })
        .collect();
    for (i, d) in declared.into_iter().enumerate() {
        let base = out.iter().position(|v| v.name == d.name);
        let path = d.path.or_else(|| base.and_then(|b| out.get(b)).map(|v| v.path.clone()));
        let size = d.size_bytes.or_else(|| base.and_then(|b| out.get(b)).map(|v| v.size_bytes));
        let v = VolumeSpec {
            name: d.name,
            path: path.ok_or_else(|| SpecError::new(&format!("volumes[{i}].path"), "required"))?,
            size_bytes: size.unwrap_or(0),
            storage_class: d.storage_class,
            mode: d.mode,
            existing: d.existing,
        };
        match base.and_then(|b| out.get_mut(b)) {
            Some(slot) => *slot = v,
            None => out.push(v),
        }
    }
    Ok(out)
}

/// Endpoints: every preset endpoint, completed by a declared one of the same name, then every
/// declared endpoint the preset does not name.
pub(super) fn endpoints(
    declared: Vec<DeclaredEndpoint>,
    preset: &[PresetEndpoint],
) -> Result<Vec<EndpointSpec>, SpecError> {
    let mut out: Vec<EndpointSpec> = preset
        .iter()
        .map(|p| EndpointSpec {
            name: p.name.clone(),
            protocol: p.protocol,
            container_port: p.port,
            exposure: p.exposure_default,
            domains: Vec::new(),
            tls: None,
            policy: NetworkPolicy::default(),
            accounting: false,
            shaping: ShapingSpec::default(),
        })
        .collect();
    for (i, d) in declared.into_iter().enumerate() {
        let at = |f: &str| format!("endpoints[{i}].{f}");
        let base = out.iter().position(|e| e.name == d.name);
        let from = base.and_then(|b| out.get(b));
        let e = EndpointSpec {
            protocol: d
                .protocol
                .or(from.map(|f| f.protocol))
                .ok_or_else(|| SpecError::new(&at("protocol"), "required"))?,
            container_port: d
                .container_port
                .or(from.map(|f| f.container_port))
                .ok_or_else(|| SpecError::new(&at("container_port"), "required"))?,
            // §3 of docs/07-networking.md: stable is the default, resolved at placement.
            exposure: d.exposure.or(from.map(|f| f.exposure)).unwrap_or(Exposure::Stable),
            name: d.name,
            domains: d.domains,
            tls: d.tls,
            policy: d.policy,
            accounting: d.accounting,
            shaping: d.shaping,
        };
        match base.and_then(|b| out.get_mut(b)) {
            Some(slot) => *slot = e,
            None => out.push(e),
        }
    }
    Ok(out)
}
