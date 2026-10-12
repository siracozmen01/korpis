// SPDX-License-Identifier: AGPL-3.0-or-later

//! `EffectService`: the effect log, read through what it concerns.

use super::wire::Refuse;
use super::wire::audit::effect_to_pb;
use super::wire::{self, time};
use super::wire::{decode_cursor, encode_cursor};
use super::{Api, error};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::audit::EffectsOf;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

impl pb::EffectService for Api {
    async fn list_effects(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListEffectsRequest>,
    ) -> ServiceResult<pb::ListEffectsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let of = match (&req.workload, &req.node) {
            (Some(w), None) => {
                EffectsOf::Workload(wire::id(IdKind::Workload, "workload", w).refuse()?)
            }
            (None, Some(n)) => EffectsOf::Node(wire::id(IdKind::Node, "node", n).refuse()?),
            _ => return Err(error::invalid("workload", "filter by one workload or one node")),
        };
        let since = req.since.as_option().map(|t| time("since", t)).transpose().refuse()?;
        let (size, token) =
            req.page.as_option().map_or((0, ""), |p| (p.page_size, p.page_token.as_str()));
        let after = if token.is_empty() { None } else { Some(decode_cursor(token)?) };
        let listed = self
            .app
            .list_effects(&caller, of, since, after, size)
            .await
            .map_err(error::from_app)?;
        Response::ok(pb::ListEffectsResponse {
            effects: listed.items.iter().map(effect_to_pb).collect(),
            page: pb::PageResponse {
                next_page_token: listed.next.map(encode_cursor).unwrap_or_default(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
    }
}
