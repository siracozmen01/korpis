// SPDX-License-Identifier: AGPL-3.0-or-later

//! `StreamService`: a workload's stream as the control plane knows it, its segment index, what
//! is held only on its node, and what is lost (§4 of docs/14-streams.md).

use super::wire::{self, Refuse};
use super::{Api, error};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_core::id::IdKind;
use korpis_core::workload::spec::streams::Retention;
use korpis_core::workload::stream::StreamKind;
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
        let kind = match request.kind.as_known() {
            None | Some(pb::StreamKind::STREAM_KIND_UNSPECIFIED) => StreamKind::Console,
            Some(_) => korpis_wire::stream::kind_from_pb("kind", request.kind).refuse()?,
        };
        let view = self.app.get_stream(&caller, workload, kind).await.map_err(error::from_app)?;
        let mut stream = korpis_wire::stream::stream_to_pb(&view.stream);
        stream.retention = Some(match view.limits.retention {
            Retention::Bytes(b) => R::RetentionBytes(b),
            Retention::Duration(d) => R::RetentionDuration(Box::new(wire::duration(d))),
        });
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
}
