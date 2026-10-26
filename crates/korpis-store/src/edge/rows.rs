// SPDX-License-Identifier: AGPL-3.0-or-later

//! The rows of edge groups and stable ports, and what they are in the model.

use crate::address::address;
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::edge::{EdgeGroup, StablePort};
use korpis_core::workload::endpoint::Transport;
use std::time::Duration;
use uuid::Uuid;

pub(super) struct GroupRow {
    pub(super) id: Uuid,
    pub(super) organization_id: Uuid,
    pub(super) name: String,
    pub(super) members: Option<Vec<Uuid>>,
    pub(super) first_port: i32,
    pub(super) last_port: i32,
    pub(super) probe_port: i32,
    pub(super) probe_interval_ms: i32,
    pub(super) probe_misses: i32,
    pub(super) term_ms: i32,
    pub(super) created_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<GroupRow> for EdgeGroup {
    type Error = StoreError;

    fn try_from(r: GroupRow) -> StoreResult<Self> {
        let corrupt = || StoreError::Corrupt(format!("edge group {}", r.id));
        let port = |p: i32| u16::try_from(p).map_err(|_| corrupt());
        let millis = |m: i32| u64::try_from(m).map(Duration::from_millis).map_err(|_| corrupt());
        let mut members = r.members.clone().unwrap_or_default();
        members.sort_unstable();
        Ok(Self {
            id: r.id,
            organization: r.organization_id,
            name: r.name.clone(),
            members,
            ports: (port(r.first_port)?, port(r.last_port)?),
            probe_port: port(r.probe_port)?,
            probe_interval: millis(r.probe_interval_ms)?,
            probe_misses: u32::try_from(r.probe_misses).map_err(|_| corrupt())?,
            term: millis(r.term_ms)?,
            created_at: crate::stored::to_time(r.created_at),
        })
    }
}

pub(super) fn groups(rows: Vec<GroupRow>) -> StoreResult<Vec<EdgeGroup>> {
    rows.into_iter().map(EdgeGroup::try_from).collect()
}

pub(super) struct PortRow {
    pub(super) address: String,
    pub(super) port: i32,
    pub(super) transport: String,
    pub(super) pool_id: Uuid,
    pub(super) workload_id: Option<Uuid>,
    pub(super) endpoint: Option<String>,
}

pub(super) fn port_of(r: PortRow) -> StoreResult<StablePort> {
    let what = format!("stable port {}:{}", r.address, r.port);
    let corrupt = || StoreError::Corrupt(what.clone());
    Ok(StablePort {
        address: address(&r.address)?,
        port: u16::try_from(r.port).map_err(|_| corrupt())?,
        transport: transport(&r.transport).ok_or_else(corrupt)?,
        pool: r.pool_id,
        workload: r.workload_id.ok_or_else(corrupt)?,
        endpoint: r.endpoint.ok_or_else(corrupt)?,
    })
}

pub(super) fn transport(t: &str) -> Option<Transport> {
    match t {
        "tcp" => Some(Transport::Tcp),
        "udp" => Some(Transport::Udp),
        _ => None,
    }
}

pub(super) fn transport_text(t: Transport) -> &'static str {
    match t {
        Transport::Tcp => "tcp",
        Transport::Udp => "udp",
    }
}

pub(super) fn millis(d: Duration) -> i32 {
    i32::try_from(d.as_millis()).unwrap_or(i32::MAX)
}
