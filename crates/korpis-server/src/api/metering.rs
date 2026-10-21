// SPDX-License-Identifier: AGPL-3.0-or-later

//! `MeteringService`: what workloads used, and the days of metering (§2 of
//! docs/15-observability.md).

use super::wire::{self, Refuse, time, timestamp};
use super::{Api, error};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::metering::{UsageOf, UsageWindow};
use korpis_app::page::Page;
use korpis_core::id::IdKind;
use korpis_core::metering::{DAY, Period, PeriodState, Record, RecordKind};
use korpis_proto::korpis::v1 as pb;

/// What the request names, exactly one of the three.
fn usage_of(req: &pb::ListUsageRequest) -> Result<UsageOf, ConnectError> {
    let named = [&req.workload, &req.project, &req.organization];
    if named.iter().filter(|n| n.is_some()).count() != 1 {
        return Err(error::invalid("workload", "name one workload, project, or organization"));
    }
    Ok(if let Some(w) = &req.workload {
        UsageOf::Workload(wire::id(IdKind::Workload, "workload", w).refuse()?)
    } else if let Some(p) = &req.project {
        UsageOf::Project(wire::id(IdKind::Project, "project", p).refuse()?)
    } else {
        let o = req.organization.as_deref().unwrap_or_default();
        UsageOf::Organization(wire::id(IdKind::Organization, "organization", o).refuse()?)
    })
}

fn kind_to_pb(k: RecordKind) -> pb::UsageKind {
    match k {
        RecordKind::Measured => pb::UsageKind::USAGE_KIND_MEASURED,
        RecordKind::Gap => pb::UsageKind::USAGE_KIND_GAP,
        RecordKind::Adjustment => pb::UsageKind::USAGE_KIND_ADJUSTMENT,
    }
}

fn record_to_pb(r: &Record) -> pb::UsageRecord {
    let losses = r.losses.iter().map(|l| pb::UsageLoss {
        from: timestamp(l.from).into(),
        to: timestamp(l.to).into(),
        cause: l.cause.clone(),
        ..Default::default()
    });
    pb::UsageRecord {
        id: wire::wire_id(IdKind::Usage, r.id),
        workload: wire::wire_id(IdKind::Workload, r.workload),
        node: r.node.map(|n| wire::wire_id(IdKind::Node, n)),
        lease_epoch: r.lease_epoch,
        from: timestamp(r.from).into(),
        to: timestamp(r.to).into(),
        kind: kind_to_pb(r.kind).into(),
        cause: r.cause.clone().unwrap_or_default(),
        supersedes: r.supersedes.iter().map(|s| wire::wire_id(IdKind::Usage, *s)).collect(),
        recorded_at: r.recorded_at.map(timestamp).into(),
        cpu_usec: r.used.cpu_usec,
        cpu_throttled_usec: r.used.cpu_throttled_usec,
        io_read_bytes: r.used.io_read_bytes,
        io_write_bytes: r.used.io_write_bytes,
        net_rx_bytes: r.used.net_rx_bytes,
        net_tx_bytes: r.used.net_tx_bytes,
        memory_kib_seconds: r.used.memory_kib_seconds,
        memory_peak_bytes: r.memory_peak_bytes,
        disk_bytes: r.disk_bytes,
        losses: losses.collect(),
        ..Default::default()
    }
}

fn period_to_pb(p: &Period) -> pb::MeteringPeriod {
    let state = match p.state {
        PeriodState::Open => pb::PeriodState::PERIOD_STATE_OPEN,
        PeriodState::Closing => pb::PeriodState::PERIOD_STATE_CLOSING,
        PeriodState::Closed => pb::PeriodState::PERIOD_STATE_CLOSED,
    };
    pb::MeteringPeriod {
        start: timestamp(p.start).into(),
        end: timestamp(p.end()).into(),
        state: state.into(),
        closes_at: timestamp(p.closes_at).into(),
        ..Default::default()
    }
}

/// The window a request names: `to` now when unset, `from` a day before `to`.
fn window(
    api: &Api,
    from: Option<&buffa_types::google::protobuf::Timestamp>,
    to: Option<&buffa_types::google::protobuf::Timestamp>,
) -> Result<(std::time::SystemTime, std::time::SystemTime), ConnectError> {
    let to = to.map(|t| time("to", t)).transpose().refuse()?.unwrap_or_else(|| api.app().now());
    let from = from.map(|t| time("from", t)).transpose().refuse()?;
    Ok((from.unwrap_or_else(|| to.checked_sub(DAY).unwrap_or(to)), to))
}

impl pb::MeteringService for Api {
    async fn list_usage(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListUsageRequest>,
    ) -> ServiceResult<pb::ListUsageResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let of = usage_of(&req)?;
        let (from, to) = window(self, req.from.as_option(), req.to.as_option())?;
        let every_version = req.every_version;
        let (size, token) =
            req.page.as_option().map_or((0, ""), |p| (p.page_size, p.page_token.as_str()));
        let after = if token.is_empty() {
            None
        } else {
            let refused = || error::invalid("page.page_token", "not a token this list gave");
            Some(self.cursors.open_usage(token).ok_or_else(refused)?)
        };
        let listed = self
            .app
            .list_usage(
                &caller,
                of,
                UsageWindow { from, to, every_version },
                Page::new(after, size),
            )
            .await
            .map_err(error::from_app)?;
        let next = listed.next.map(|n| self.cursors.seal_usage(n)).transpose()?;
        Response::ok(pb::ListUsageResponse {
            records: listed.items.iter().map(record_to_pb).collect(),
            page: pb::PageResponse {
                next_page_token: next.unwrap_or_default(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
    }

    async fn list_periods(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListPeriodsRequest>,
    ) -> ServiceResult<pb::ListPeriodsResponse> {
        self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let (from, to) = window(self, req.from.as_option(), req.to.as_option())?;
        let periods = self.app.metering_periods(from, to).await.map_err(error::from_app)?;
        Response::ok(pb::ListPeriodsResponse {
            periods: periods.iter().map(period_to_pb).collect(),
            ..Default::default()
        })
    }
}
