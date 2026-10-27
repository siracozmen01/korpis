// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's stable addresses and ports, given where it is placed (§3.1 of
//! docs/07-networking.md): under the placement lock, in the transaction that binds it, from the
//! pools of the group serving it, which is the group of what it holds already where that still
//! serves, and otherwise the nearest organization's group with room. They are kept for as long
//! as the workload declares the endpoint, across every move and stop, and given back when it is
//! retired or stops declaring it.

use crate::App;
use crate::error::AppError;
use crate::ports::{AddressWrite as _, Clock, EdgeWrite as _, RecipeSource, Store};
use korpis_core::address::AddressPool;
use korpis_core::edge::{EdgeGroup, StablePort, allocate, alongside};
use korpis_core::workload::endpoint::Transport;
use korpis_core::workload::spec::IntentBody;
use korpis_core::workload::spec::endpoints::Exposure;
use std::net::IpAddr;
use std::time::SystemTime;
use uuid::Uuid;

/// An endpoint declared `stable`: its name, container port, and transport.
pub(crate) type Wanted = (String, u16, Transport);

/// The endpoints of `body` declared `stable`, in declared order.
pub(crate) fn stable_endpoints(body: &IntentBody) -> Vec<Wanted> {
    let stable = body.endpoints.iter().filter(|e| e.exposure == Exposure::Stable);
    stable.map(|e| (e.name.clone(), e.container_port, e.protocol.transport())).collect()
}

/// The group serving a workload's `stable` endpoints, and everything they are to hold of its
/// pools: what they hold already, and what is given now.
#[derive(Debug, Clone)]
pub(crate) struct Serving {
    /// The group.
    pub(crate) group: EdgeGroup,
    /// What the endpoints are to hold.
    pub(crate) ports: Vec<StablePort>,
}

/// What a workload's endpoints are to hold of `pools`, beside `held`, at `now`.
struct Allot<'a> {
    workload: Uuid,
    wanted: &'a [Wanted],
    held: &'a [StablePort],
    now: SystemTime,
}

/// What a placement gives a workload of the operator's addresses: the nodes it may hold a
/// dedicated address on (§3.7 of docs/07-networking.md), and the groups serving its `stable` and
/// its `ingress` endpoints (§3.1 there).
pub(crate) struct Addressing {
    /// The nodes it may hold a dedicated address on.
    pub(crate) dedicated: std::collections::BTreeSet<Uuid>,
    /// The group serving its `stable` endpoints, and what they are to hold.
    pub(crate) edge: Option<Serving>,
    /// Whether, no group serving them, its `stable` endpoints are reached at a `cluster`
    /// pool's address it holds, which its router moves (§3.1 of docs/07-networking.md).
    pub(crate) routed: bool,
    /// The group serving its `ingress` endpoints, and that group's ingress addresses.
    pub(crate) ingress: Option<super::ingress::IngressServing>,
    /// The members of the groups serving it.
    members: Vec<Uuid>,
}

impl Addressing {
    /// The members of the groups serving it, which a node it is placed on reaches through the
    /// mesh; none where none does.
    pub(crate) fn edge_members(&self) -> &[Uuid] {
        &self.members
    }

    /// Whether a declared `stable` endpoint of it resolves to `stable`: a group serves it, or
    /// it is routed.
    pub(crate) fn stable(&self) -> bool {
        self.edge.is_some() || self.routed
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// What placing `workload` for `body` among `nodes` gives it of the operator's addresses.
    pub(crate) async fn addressing(
        &self,
        tx: &mut S::Tx,
        (workload, body): (Uuid, &IntentBody),
        owners: &[Uuid],
        nodes: &[korpis_core::node::Node],
    ) -> Result<Addressing, AppError> {
        let edge = self.edge_serving(tx, (workload, body), owners).await?;
        let ingress = self.ingress_serving(tx, (workload, body), owners, edge.as_ref()).await?;
        let routed = edge.is_none()
            && !stable_endpoints(body).is_empty()
            && self.routable(tx, workload, owners).await?;
        let dedicated = self.dedicated_nodes(tx, (workload, body, routed), owners, nodes).await?;
        let groups = edge.iter().map(|e| &e.group).chain(ingress.iter().map(|i| &i.group));
        let mut members: Vec<Uuid> = groups.flat_map(|g| g.members.iter().copied()).collect();
        members.sort_unstable();
        members.dedup();
        Ok(Addressing { dedicated, edge, routed, ingress, members })
    }

    /// Records what binding `workload` to `node` gives it: its dedicated address, its stable
    /// addresses and ports, and the group serving its ingress.
    pub(crate) async fn hold_addressing(
        &self,
        tx: &mut S::Tx,
        (workload, body): (Uuid, &IntentBody),
        (owners, node): (&[Uuid], &korpis_core::node::Node),
        addressing: &Addressing,
    ) -> Result<(), AppError> {
        self.hold_dedicated(tx, (workload, body, addressing.routed), owners, node).await?;
        self.hold_stable(tx, (workload, body), addressing.edge.as_ref()).await?;
        self.hold_ingress(tx, workload, addressing.ingress.as_ref()).await
    }

    /// Whether a declared `stable` endpoint of a workload of `organization` resolves to
    /// `stable`, which is what a plan says before a placement allocates its address: an edge
    /// group of it or an organization above it with two members or more and a pool serves it,
    /// or a `cluster` pool of one of them does, at whose address it is routed.
    pub(crate) async fn edge_serves(&self, organization: Uuid) -> Result<bool, AppError> {
        let owners = self.owners(organization).await?;
        for g in self.store.edge_groups_all().await? {
            if g.serves()
                && owners.contains(&g.organization)
                && self.store.edge_group_named(g.id).await?
            {
                return Ok(true);
            }
        }
        let mut tx = self.store.begin().await?;
        for org in &owners {
            if tx.address_pools_now(*org).await?.iter().any(AddressPool::is_cluster) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The endpoints of `workload`, declared as `body` says, that hold no port of its node for
    /// being `stable`: those an edge serves now, holding an address and port of a pool whose
    /// group has two members or more; and, where it serves none, every `stable` one where the
    /// workload holds a `cluster` pool's address, at which they are routed (§3.1 of
    /// docs/07-networking.md).
    pub(crate) async fn stable_served(
        &self,
        workload: Uuid,
        body: &IntentBody,
    ) -> Result<Vec<String>, AppError> {
        let mut out: Vec<String> = Vec::new();
        for p in self.store.stable_of(workload).await? {
            let pool = self.store.address_pool(p.pool).await?;
            let Some(group) = pool.and_then(|p| p.group()) else { continue };
            let serves = self.store.edge_group(group).await?.is_some_and(|g| g.serves());
            if serves && !out.contains(&p.endpoint) {
                out.push(p.endpoint);
            }
        }
        if out.is_empty() && self.store.dedicated_of(workload).await?.is_some_and(|h| h.cluster) {
            out = stable_endpoints(body).into_iter().map(|(name, _, _)| name).collect();
        }
        Ok(out)
    }

    /// The edge pools of `owners`, by the group naming each.
    async fn edge_pools(
        &self,
        tx: &mut S::Tx,
        owners: &[Uuid],
    ) -> Result<Vec<AddressPool>, AppError> {
        let mut out = Vec::new();
        for org in owners {
            let pools = tx.address_pools_now(*org).await?;
            out.extend(pools.into_iter().filter(|p| p.group().is_some()));
        }
        Ok(out)
    }

    /// Everything `a` is to hold of `g`'s pools: the IPv4 address and ports each endpoint holds
    /// there or is given on the address it holds, or failing that the lowest with room; and the
    /// same ports at an IPv6 address beside, where the group has an IPv6 pool with room. None
    /// where no IPv4 pool has room for them all.
    async fn allot(
        &self,
        tx: &mut S::Tx,
        (g, pools): (&EdgeGroup, &[AddressPool]),
        a: &Allot<'_>,
    ) -> Result<Option<Vec<StablePort>>, AppError> {
        let ours = |p: &StablePort| pools.iter().any(|q| q.id == p.pool);
        let mut out: Vec<StablePort> = a.held.iter().filter(|p| ours(p)).cloned().collect();
        let has = |out: &[StablePort], name: &str, v4: bool| {
            out.iter().any(|p| p.endpoint == name && p.address.is_ipv4() == v4)
        };
        let missing: Vec<&Wanted> =
            a.wanted.iter().filter(|(n, _, _)| !has(&out, n, true)).collect();
        if !missing.is_empty() {
            let prefer = out.iter().find(|p| p.address.is_ipv4()).map(|p| p.address);
            let asked: Vec<(u16, Transport)> = missing.iter().map(|(_, p, t)| (*p, *t)).collect();
            let given = self.room_v4(tx, (g, pools), &asked, prefer, a.now).await?;
            let Some((pool, got)) = given else { return Ok(None) };
            for ((name, _, transport), port) in missing.iter().zip(got.ports) {
                out.push(StablePort {
                    address: got.address,
                    port,
                    transport: *transport,
                    pool,
                    workload: a.workload,
                    endpoint: name.clone(),
                });
            }
        }
        self.allot_v6(tx, pools, a, &mut out).await?;
        Ok(Some(out))
    }

    /// The first IPv4 pool of `pools` with room for `asked` on one address, and what it gives.
    async fn room_v4(
        &self,
        tx: &mut S::Tx,
        (g, pools): (&EdgeGroup, &[AddressPool]),
        asked: &[(u16, Transport)],
        prefer: Option<IpAddr>,
        now: SystemTime,
    ) -> Result<Option<(Uuid, korpis_core::edge::Given)>, AppError> {
        for p in pools.iter().filter(|p| p.cidr.addr().is_ipv4()) {
            let taken = tx.stable_taken(p.id).await?;
            if let Some(got) = allocate((p, &taken), asked, (g.ports, prefer), now) {
                return Ok(Some((p.id, got)));
            }
        }
        Ok(None)
    }

    /// Gives each endpoint holding an IPv4 port and no IPv6 one the same port at an IPv6 address
    /// of `pools`, where one has room for them all.
    async fn allot_v6(
        &self,
        tx: &mut S::Tx,
        pools: &[AddressPool],
        a: &Allot<'_>,
        out: &mut Vec<StablePort>,
    ) -> Result<(), AppError> {
        let has_v6 = |name: &str, out: &[StablePort]| {
            out.iter().any(|p| p.endpoint == name && p.address.is_ipv6())
        };
        let lacking: Vec<StablePort> = out
            .iter()
            .filter(|p| p.address.is_ipv4() && !has_v6(&p.endpoint, out))
            .cloned()
            .collect();
        if lacking.is_empty() {
            return Ok(());
        }
        let ports: Vec<(u16, Transport)> = lacking.iter().map(|p| (p.port, p.transport)).collect();
        let prefer = out.iter().find(|p| p.address.is_ipv6()).map(|p| p.address);
        for p in pools.iter().filter(|p| p.cidr.addr().is_ipv6()) {
            let taken = tx.stable_taken(p.id).await?;
            if let Some(address) = alongside((p, &taken), &ports, prefer, a.now) {
                let beside = lacking.into_iter().map(|l| StablePort { address, pool: p.id, ..l });
                out.extend(beside);
                return Ok(());
            }
        }
        Ok(())
    }

    /// The group serving `workload`'s `stable` endpoints as `body` declares them, and what they
    /// are to hold of it: the group of what it holds already, where that still serves and has
    /// room, and otherwise the first group of the nearest of `owners` that serves and has room.
    /// None where `body` declares none, or no group serves it.
    pub(crate) async fn edge_serving(
        &self,
        tx: &mut S::Tx,
        (workload, body): (Uuid, &IntentBody),
        owners: &[Uuid],
    ) -> Result<Option<Serving>, AppError> {
        let wanted = stable_endpoints(body);
        if wanted.is_empty() {
            return Ok(None);
        }
        let held = tx.stable_now(workload).await?;
        let pools = self.edge_pools(tx, owners).await?;
        let mut groups = tx.edge_groups_now(owners).await?;
        let nearness = |g: &EdgeGroup| owners.iter().position(|o| *o == g.organization);
        let holds = |g: &EdgeGroup| {
            let of_g =
                |p: &StablePort| pools.iter().any(|q| q.id == p.pool && q.group() == Some(g.id));
            held.iter().any(of_g)
        };
        groups.sort_by_key(|g| (!holds(g), nearness(g), g.id));
        let a = Allot { workload, wanted: &wanted, held: &held, now: self.clock.now() };
        for g in groups.into_iter().filter(EdgeGroup::serves) {
            let mine: Vec<AddressPool> =
                pools.iter().filter(|p| p.group() == Some(g.id)).cloned().collect();
            if let Some(ports) = self.allot(tx, (&g, &mine), &a).await? {
                return Ok(Some(Serving { group: g, ports }));
            }
        }
        Ok(None)
    }

    /// Records what `serving` gives `workload`'s endpoints, and gives back what an endpoint holds
    /// that `body` no longer declares `stable`, or that another group's pool gives it in place of;
    /// what a declared one holds is kept where no group serves it now, since its address may
    /// serve again.
    pub(crate) async fn hold_stable(
        &self,
        tx: &mut S::Tx,
        (workload, body): (Uuid, &IntentBody),
        serving: Option<&Serving>,
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        let keep: Vec<String> = stable_endpoints(body).into_iter().map(|(n, _, _)| n).collect();
        tx.release_stable(workload, &keep, now).await?;
        let Some(serving) = serving else { return Ok(()) };
        let held = tx.stable_now(workload).await?;
        let key = |p: &StablePort| (p.address, p.port, p.transport);
        let kept: Vec<(IpAddr, u16, Transport)> = serving.ports.iter().map(key).collect();
        for p in held.iter().filter(|p| !kept.contains(&key(p))) {
            tx.release_stable_port(key(p), now).await?;
        }
        let had: Vec<(IpAddr, u16, Transport)> = held.iter().map(key).collect();
        for p in serving.ports.iter().filter(|p| !had.contains(&key(p))) {
            tx.hold_stable(p, now).await?;
        }
        Ok(())
    }
}
