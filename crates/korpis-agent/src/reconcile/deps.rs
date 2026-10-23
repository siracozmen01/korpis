// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's dependencies, the node's half (§5 of docs/07-networking.md). The control plane
//! said which endpoints each reaches; the node says where they are, from the addresses it gave
//! their workloads, holds a dependent back while a required dependency is not running here and
//! ready, injects the addresses into its environment at creation and into a file it keeps
//! current, and opens each in the dependent's chain past the egress default.

use super::Reconciler;
use super::act::running;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::net::Ipv4Addr;
use std::path::Path;

/// One endpoint a dependency reaches, at the address this node gave its workload.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reached {
    workload: String,
    endpoint: String,
    host: Ipv4Addr,
    port: u16,
    protocol: pb::EndpointProtocol,
}

fn protocol_name(p: pb::EndpointProtocol) -> &'static str {
    match p {
        pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP => "udp",
        pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP => "http",
        pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTPS => "https",
        pb::EndpointProtocol::ENDPOINT_PROTOCOL_QUIC => "quic",
        _ => "tcp",
    }
}

fn transport(p: pb::EndpointProtocol) -> helper::Transport {
    match p {
        pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP
        | pb::EndpointProtocol::ENDPOINT_PROTOCOL_QUIC => helper::Transport::TRANSPORT_UDP,
        _ => helper::Transport::TRANSPORT_TCP,
    }
}

fn injects(d: &agent::ResolvedDependency) -> (bool, bool) {
    match d.inject.as_known() {
        Some(pb::DependencyInjection::DEPENDENCY_INJECTION_ENV) => (true, false),
        Some(pb::DependencyInjection::DEPENDENCY_INJECTION_FILE) => (false, true),
        _ => (true, true),
    }
}

/// The variables of `d` reaching `reached`: the first's host and port, and every one.
fn variables(d: &agent::ResolvedDependency, reached: &[Reached]) -> Vec<(String, String)> {
    let Some(first) = reached.first() else { return Vec::new() };
    let prefix = format!("KORPIS_DEP_{}", d.name.to_ascii_uppercase());
    let all: Vec<String> = reached.iter().map(|r| format!("{}:{}", r.host, r.port)).collect();
    vec![
        (format!("{prefix}_HOST"), first.host.to_string()),
        (format!("{prefix}_PORT"), first.port.to_string()),
        (format!("{prefix}_ADDRESSES"), all.join(",")),
    ]
}

/// The file of `d` reaching `reached`.
fn document(d: &agent::ResolvedDependency, reached: &[Reached]) -> String {
    let addresses: Vec<serde_json::Value> = reached
        .iter()
        .map(|r| {
            serde_json::json!({"workload": r.workload, "endpoint": r.endpoint,
                "host": r.host.to_string(), "port": r.port, "protocol": protocol_name(r.protocol)})
        })
        .collect();
    let doc = serde_json::json!({"name": d.name, "addresses": addresses});
    format!("{doc}\n")
}

/// Writes `text` to `path` unless it holds it already, readable by every user, since the
/// container's users are not the host's.
fn keep(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if std::fs::read_to_string(path).is_ok_and(|held| held == text) {
        return Ok(());
    }
    let staged = path.with_extension("json.new");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&staged)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(staged, path)
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// The endpoints `d` reaches that are on this node, where their workloads have addresses.
    fn reached(&self, d: &agent::ResolvedDependency) -> Vec<Reached> {
        let Some(net) = self.wiring.network.as_ref() else { return Vec::new() };
        d.targets
            .iter()
            .filter_map(|t| {
                Some(Reached {
                    workload: t.workload.clone(),
                    endpoint: t.endpoint.clone(),
                    host: net.addresses.of(&t.workload)?.ipv4,
                    port: u16::try_from(t.container_port).ok()?,
                    protocol: t.protocol.as_known().unwrap_or_default(),
                })
            })
            .collect()
    }

    /// The first required dependency of `a` whose targets are not all running on this node and
    /// ready, by name: a target with a readiness probe is ready once it passes (§5 of
    /// docs/07-networking.md).
    fn waiting_for(&self, a: &agent::Assignment) -> Option<String> {
        a.dependencies.iter().filter(|d| d.required).find_map(|d| {
            let reached = self.reached(d);
            let up = |r: &Reached| {
                let t = self.tracked.get(&r.workload);
                t.is_some_and(|t| t.status.as_ref().is_some_and(running) && t.probing.ready())
            };
            (reached.is_empty() || !reached.iter().all(up)).then(|| d.name.clone())
        })
    }

    /// Whether `a`'s object waits to be made for a required dependency, which `t` then says.
    pub(super) fn held_back(&self, a: &agent::Assignment, t: &mut super::Tracked) -> bool {
        t.waiting = self.waiting_for(a);
        t.waiting.is_some()
    }

    /// What `a`'s dependencies open past the egress default: each endpoint they reach here.
    pub(super) fn dependency_destinations(
        &self,
        a: &agent::Assignment,
    ) -> Vec<helper::Destination> {
        let mut out: Vec<helper::Destination> = a
            .dependencies
            .iter()
            .flat_map(|d| self.reached(d))
            .map(|r| helper::Destination {
                address: r.host.to_string(),
                port: u32::from(r.port),
                transport: transport(r.protocol).into(),
                reason: helper::DestinationReason::DESTINATION_REASON_DEPENDENCY.into(),
                ..Default::default()
            })
            .collect();
        out.sort_by(|x, y| (&x.address, x.port).cmp(&(&y.address, y.port)));
        out.dedup();
        out
    }

    /// Sets `a`'s dependencies' variables in `spec`'s environment.
    pub(super) fn inject(&self, a: &agent::Assignment, spec: &mut driver::PreparedSpec) {
        for d in a.dependencies.iter().filter(|d| injects(d).0) {
            spec.env.extend(variables(d, &self.reached(d)));
        }
    }

    /// Writes `a`'s dependencies' files into `dir/deps`, the workload's own directory that it
    /// sees as `/run/korpis`, and removes those of dependencies it no longer declares.
    pub(super) fn write_dependencies(
        &self,
        a: &agent::Assignment,
        dir: &Path,
    ) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let deps = dir.join("deps");
        let filed: Vec<_> = a.dependencies.iter().filter(|d| injects(d).1).collect();
        if filed.is_empty() && !deps.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&deps)?;
        std::fs::set_permissions(&deps, std::fs::Permissions::from_mode(0o755))?;
        for d in &filed {
            keep(&deps.join(format!("{}.json", d.name)), &document(d, &self.reached(d)))?;
        }
        for entry in std::fs::read_dir(&deps)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let declared =
                name.strip_suffix(".json").is_some_and(|n| filed.iter().any(|d| d.name == n));
            if !declared {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    /// Rewrites `a`'s dependencies' files where its directory is served.
    fn refresh_files(&self, a: &agent::Assignment) {
        let Some(identity) = &self.wiring.identity else { return };
        let dir = identity.dir(&a.workload);
        if !dir.exists() {
            return;
        }
        if let Err(e) = self.write_dependencies(a, &dir) {
            tracing::warn!(workload = %a.workload, error = %e, "its dependencies' files could not be written");
        }
    }

    /// Keeps every running workload's dependencies current: its files rewritten, and its network
    /// made again where what its dependencies reach changed, so a workload that watches its file
    /// reaches a new address at once. Its environment changes at its next start.
    pub(super) async fn refresh_dependencies(&mut self, held: &crate::held::HeldSet) {
        let assignments: Vec<agent::Assignment> =
            held.assignments.values().filter(|a| !a.dependencies.is_empty()).cloned().collect();
        for a in assignments {
            if self.tracked.get(&a.workload).is_none_or(|t| t.runtime.is_none()) {
                continue;
            }
            self.refresh_files(&a);
            let opened =
                self.wiring.network.as_ref().and_then(|n| n.dependencies_opened.get(&a.workload));
            if opened.is_some_and(|o| *o != self.dependency_destinations(&a))
                && let Err(e) = self.ensure_network(&a).await
            {
                tracing::warn!(workload = %a.workload, error = %e.0, "its dependencies could not be opened");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency(inject: pb::DependencyInjection) -> agent::ResolvedDependency {
        agent::ResolvedDependency { name: "db".into(), inject: inject.into(), ..Default::default() }
    }

    fn reached(host: [u8; 4], port: u16) -> Reached {
        Reached {
            workload: "wl_1".into(),
            endpoint: "pg".into(),
            host: host.into(),
            port,
            protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP,
        }
    }

    #[test]
    fn the_first_target_is_the_host_and_port_and_every_one_is_listed() {
        let d = dependency(pb::DependencyInjection::DEPENDENCY_INJECTION_BOTH);
        let both = [reached([10, 47, 0, 2], 5432), reached([10, 47, 0, 3], 5432)];
        let vars = variables(&d, &both);
        assert_eq!(vars[0], ("KORPIS_DEP_DB_HOST".to_owned(), "10.47.0.2".to_owned()));
        assert_eq!(vars[1], ("KORPIS_DEP_DB_PORT".to_owned(), "5432".to_owned()));
        assert_eq!(vars[2].1, "10.47.0.2:5432,10.47.0.3:5432");
        assert!(variables(&d, &[]).is_empty(), "none reached, no variables");
        let doc: serde_json::Value = serde_json::from_str(&document(&d, &both[..1])).unwrap();
        assert_eq!(doc["addresses"][0]["host"], "10.47.0.2");
        assert_eq!(doc["addresses"][0]["protocol"], "tcp");
        let none: serde_json::Value = serde_json::from_str(&document(&d, &[])).unwrap();
        assert_eq!(none["addresses"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn injection_is_what_the_dependency_asks() {
        let env = dependency(pb::DependencyInjection::DEPENDENCY_INJECTION_ENV);
        let file = dependency(pb::DependencyInjection::DEPENDENCY_INJECTION_FILE);
        let unset = dependency(pb::DependencyInjection::DEPENDENCY_INJECTION_UNSPECIFIED);
        assert_eq!(
            (injects(&env), injects(&file), injects(&unset)),
            ((true, false), (false, true), (true, true))
        );
    }
}
