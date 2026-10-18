// SPDX-License-Identifier: AGPL-3.0-or-later

//! `StreamService`: a workload's stream as the control plane knows it, its segment index, what
//! is held only on its node, and what is lost (§4 of docs/14-streams.md).

use super::wire::{self, Refuse};
use super::{Api, error, stream_offloaded};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream};
use korpis_core::id::IdKind;
use korpis_core::workload::spec::streams::Retention;
use korpis_core::workload::stream::{StreamKey, StreamKind};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::stream::Retention as R;

impl pb::StreamService for Api {
    async fn get_stream(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetStreamRequest>,
    ) -> ServiceResult<pb::GetStreamResponse> {
        let caller = self.caller(&ctx).await?;
        let workload = wire::id(IdKind::Workload, "workload", request.workload).refuse()?;
        let kind = kind("kind", request.kind)?;
        let key = StreamKey { workload, kind, run: optional_run(request.run)? };
        let view = self.app.get_stream(&caller, key).await.map_err(error::from_app)?;
        let mut stream = korpis_wire::stream::stream_to_pb(&view.stream);
        stream.retention = Some(match view.limits.retention {
            Retention::Bytes(b) => R::RetentionBytes(b),
            Retention::Duration(d) => R::RetentionDuration(Box::new(wire::duration(d))),
        });
        stream.offload_retention = wire::duration(view.limits.offload_kept(view.floor)).into();
        stream.retention_floor = wire::duration(view.floor).into();
        let lost = view.stream.lost().into_iter().map(|(first, end)| pb::OffsetRange {
            first,
            end,
            ..Default::default()
        });
        Response::ok(pb::GetStreamResponse {
            stream: stream.into(),
            lost: lost.collect(),
            ..Default::default()
        })
    }

    async fn read_offloaded(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ReadOffloadedRequest>,
    ) -> ServiceResult<ServiceStream<pb::ConsoleChunk>> {
        let caller = self.caller(&ctx).await?;
        let workload = wire::id(IdKind::Workload, "workload", request.workload).refuse()?;
        let kind = kind("kind", request.kind)?;
        let from = request.from_offset;
        let run = optional_run(request.run)?;
        let read = self.app.read_offloaded(&caller, StreamKey { workload, kind, run }, from).await;
        let read = read.map_err(error::from_app)?;
        let repos = stream_offloaded::opened(&read)?;
        Response::stream_ok(stream_offloaded::chunks(read, repos, (from, request.limit)))
    }
}

/// The run named in `run`, if one is: a run's console is a stream of its own.
fn optional_run(run: &str) -> Result<Option<uuid::Uuid>, connectrpc::ConnectError> {
    (!run.is_empty()).then(|| wire::id(IdKind::Run, "run", run)).transpose().refuse()
}

/// The stream kind asked for in `field`: the console when unspecified.
fn kind(
    field: &str,
    k: buffa::EnumValue<pb::StreamKind>,
) -> Result<StreamKind, connectrpc::ConnectError> {
    match k.as_known() {
        None | Some(pb::StreamKind::STREAM_KIND_UNSPECIFIED) => Ok(StreamKind::Console),
        Some(_) => korpis_wire::stream::kind_from_pb(field, k).refuse(),
    }
}
