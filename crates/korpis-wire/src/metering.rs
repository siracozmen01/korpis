// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metering samples: `korpis.agent.v1.MeteringSample` (§2 of docs/15-observability.md),
//! translated strictly; whether to believe one is the ingesting use case's.

use crate::{Result, duration_from, id, invalid, time};
use korpis_core::id::IdKind;
use korpis_core::metering::{Counters, EndpointUse, Loss, Sample};
use korpis_proto::korpis::agent::v1 as agent;

fn counters(field: &str, c: &agent::MeterCounters) -> Result<Counters> {
    let names: std::collections::BTreeSet<&str> =
        c.endpoints.iter().map(|e| e.endpoint.as_str()).collect();
    if names.len() != c.endpoints.len() || names.contains("") {
        return Err(invalid(&format!("{field}.endpoints"), "each endpoint is named, once"));
    }
    let mut device_seconds = korpis_core::metering::DeviceSeconds::new();
    for (i, d) in c.device_seconds.iter().enumerate() {
        let class = crate::node::device_from_pb(&format!("{field}.device_seconds[{i}]"), d.class)?;
        if device_seconds.insert(class, d.seconds).is_some() {
            return Err(invalid(&format!("{field}.device_seconds"), "each class once"));
        }
    }
    Ok(Counters {
        dedicated_core_seconds: c.dedicated_core_seconds,
        device_seconds,
        cpu_usec: c.cpu_usec,
        cpu_throttled_usec: c.cpu_throttled_usec,
        io_read_bytes: c.io_read_bytes,
        io_write_bytes: c.io_write_bytes,
        net_rx_bytes: c.net_rx_bytes,
        net_tx_bytes: c.net_tx_bytes,
        memory_kib_seconds: c.memory_kib_seconds,
        egress_bytes: c.egress_bytes,
        endpoints: c
            .endpoints
            .iter()
            .map(|e| {
                let used = EndpointUse {
                    received_bytes: e.received_bytes,
                    received_packets: e.received_packets,
                    sent_bytes: e.sent_bytes,
                    sent_packets: e.sent_packets,
                };
                (e.endpoint.clone(), used)
            })
            .collect(),
    })
}

fn loss(field: &str, l: &agent::MeterLoss) -> Result<Loss> {
    let at = |name: &str, t: Option<&buffa_types::google::protobuf::Timestamp>| {
        let field = format!("{field}.{name}");
        time(&field, t.ok_or_else(|| invalid(&field, "required"))?)
    };
    Ok(Loss {
        from: at("from", l.from.as_option())?,
        to: at("to", l.to.as_option())?,
        cause: l.cause.clone(),
    })
}

/// A sample, as its node journaled it.
pub fn sample_from_pb(field: &str, s: &agent::MeteringSample) -> Result<Sample> {
    let start = format!("{field}.interval_start");
    let start =
        time(&start, s.interval_start.as_option().ok_or_else(|| invalid(&start, "required"))?)?;
    let length = format!("{field}.interval");
    let interval = duration_from(
        &length,
        s.interval.as_option().ok_or_else(|| invalid(&length, "required"))?,
    )?;
    if interval.is_zero() {
        return Err(invalid(&length, "an interval has a length"));
    }
    let losses = s.losses.iter().enumerate().map(|(i, l)| loss(&format!("{field}.losses[{i}]"), l));
    Ok(Sample {
        workload: id(IdKind::Workload, &format!("{field}.workload"), &s.workload)?,
        interval_start: start,
        interval,
        lease_epoch: s.lease_epoch,
        counters: match s.counters.as_option() {
            Some(c) => counters(&format!("{field}.counters"), c)?,
            None => Counters::default(),
        },
        memory_peak_bytes: s.memory_peak_bytes,
        disk_bytes: s.disk_bytes,
        disk_allocated: s.disk_allocated,
        losses: losses.collect::<Result<_>>()?,
    })
}
