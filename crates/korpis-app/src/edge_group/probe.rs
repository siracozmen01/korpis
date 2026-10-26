// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's probe of each group's members through their forwarding paths (§3.1 of
//! docs/07-networking.md): which replica probes a group, what it probes, and recording what it
//! measured. A replica holds a group's probe for a while, as a route is held (§3.7 there), so one
//! replica probes each group and another takes over once it stops renewing.

use crate::App;
use crate::edge_group::EdgeTables;
use crate::error::AppError;
use crate::ports::{Clock, EdgeLeaseWrite, RecipeSource, Store, Tx};
use korpis_core::edge::lease::MemberState;
use korpis_core::edge::probe::AddressStatus;
use korpis_core::id::{Id, IdKind};
use std::net::IpAddr;
use std::time::Duration;
use uuid::Uuid;

/// What a replica probes of one group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbePlan {
    /// The group.
    pub group: Uuid,
    /// The port each member forwards for the probe.
    pub port: u16,
    /// Between two probes of a member.
    pub interval: Duration,
    /// Missed in a row before a member fails.
    pub misses: u32,
    /// Each member.
    pub targets: Vec<ProbeTarget>,
}

/// What a replica probes of one member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTarget {
    /// The member.
    pub member: Uuid,
    /// Its own first advertised IPv4 address, or its first IPv6 address where it advertises no
    /// IPv4 one; none where it advertises none.
    pub own: Option<IpAddr>,
    /// Each address it holds now.
    pub held: Vec<IpAddr>,
    /// The node its probe is forwarded to, by wire identifier, with that node's public key; none
    /// where the group forwards it to none yet.
    pub answerer: Option<(String, Vec<u8>)>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Makes replica `replica` the prober, for `hold_for`, of every group no other replica
    /// probes, renewing those it probes; the groups it probes.
    ///
    /// # Errors
    /// The store failed.
    pub async fn claim_edge_probes(
        &self,
        replica: Uuid,
        hold_for: Duration,
    ) -> Result<Vec<Uuid>, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let groups = tx.claim_edge_probes(replica, (now, now + hold_for)).await?;
        tx.commit().await?;
        Ok(groups)
    }

    /// What to probe of `group`, as the store holds it and `tables` tell each member where its
    /// probe goes; none where the group is gone.
    ///
    /// # Errors
    /// The store failed.
    pub async fn edge_probe_plan(
        &self,
        group: Uuid,
        tables: &EdgeTables,
    ) -> Result<Option<ProbePlan>, AppError> {
        let Some(g) = self.store.edge_group(group).await? else { return Ok(None) };
        let addresses = self.store.edge_addresses_of(group).await?;
        let now = self.clock.now();
        let mut targets = Vec::with_capacity(g.members.len());
        for &member in &g.members {
            let node = self.store.node(member).await?;
            let advertised = node.and_then(|n| n.advertised).map(|a| a.addresses);
            let own = advertised.and_then(|a| {
                let v4 = a.iter().copied().find(IpAddr::is_ipv4);
                v4.or_else(|| a.first().copied())
            });
            let held = addresses
                .iter()
                .filter(|a| a.hold.holder(now) == Some(member))
                .map(|a| a.hold.address)
                .collect();
            let forwarded =
                tables.of(member).and_then(|t| t.groups.iter().find(|f| f.group == group));
            let answerer = match forwarded.and_then(|f| f.probe) {
                Some((n, _)) => self
                    .store
                    .node_key(n)
                    .await?
                    .map(|key| (Id::from_uuid(IdKind::Node, n).to_string(), key)),
                None => None,
            };
            targets.push(ProbeTarget { member, own, held, answerer });
        }
        Ok(Some(ProbePlan {
            group,
            port: g.probe_port,
            interval: g.probe_interval,
            misses: g.probe_misses,
            targets,
        }))
    }

    /// What `group`'s status shows: each member as the control plane knows it, and each address
    /// with a lease. The caller has read the group already.
    ///
    /// # Errors
    /// The store failed.
    pub async fn edge_group_status(
        &self,
        group: Uuid,
    ) -> Result<(Vec<MemberState>, Vec<AddressStatus>), AppError> {
        let mut members = self.store.edge_members_all().await?;
        members.retain(|m| m.group == group);
        Ok((members, self.store.edge_addresses_of(group).await?))
    }

    /// Records `member`'s own probe of `group`: whether it passes and, where it does not, why.
    ///
    /// # Errors
    /// The store failed.
    pub async fn record_member_probe(
        &self,
        (group, member): (Uuid, Uuid),
        (passes, reason): (bool, Option<&str>),
    ) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.record_member_probe((group, member), (passes, reason), self.clock.now()).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records whether `address` answers its probe through its holder.
    ///
    /// # Errors
    /// The store failed.
    pub async fn record_address_probe(
        &self,
        address: IpAddr,
        answering: bool,
    ) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.record_address_probe(address, answering, self.clock.now()).await?;
        tx.commit().await?;
        Ok(())
    }
}
