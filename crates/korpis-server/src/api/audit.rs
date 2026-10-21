// SPDX-License-Identifier: AGPL-3.0-or-later

//! `EffectService`: the effect log, read through what it concerns (§4 of
//! docs/15-observability.md).

use super::wire::Refuse;
use super::wire::audit::effect_to_pb;
use super::wire::{self, time};
use super::{Api, error};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::audit::{EffectQuery, EffectsOf};
use korpis_app::page::Page;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

/// What the request names, exactly one of the five.
fn effects_of(req: &pb::ListEffectsRequest) -> Result<EffectsOf, ConnectError> {
    let named = [&req.workload, &req.plan, &req.node, &req.organization, &req.project];
    if named.iter().filter(|n| n.is_some()).count() != 1 {
        return Err(error::invalid(
            "workload",
            "name one workload, plan, node, project, or organization",
        ));
    }
    Ok(if let Some(w) = &req.workload {
        EffectsOf::Workload(wire::id(IdKind::Workload, "workload", w).refuse()?)
    } else if let Some(p) = &req.plan {
        EffectsOf::Plan(wire::id(IdKind::Plan, "plan", p).refuse()?)
    } else if let Some(n) = &req.node {
        EffectsOf::Node(wire::id(IdKind::Node, "node", n).refuse()?)
    } else if let Some(p) = &req.project {
        EffectsOf::Project(wire::id(IdKind::Project, "project", p).refuse()?)
    } else {
        let o = req.organization.as_deref().unwrap_or_default();
        EffectsOf::Organization(wire::id(IdKind::Organization, "organization", o).refuse()?)
    })
}

impl pb::EffectService for Api {
    async fn list_effects(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListEffectsRequest>,
    ) -> ServiceResult<pb::ListEffectsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let of = effects_of(&req)?;
        let since = req.since.as_option().map(|t| time("since", t)).transpose().refuse()?;
        let prefix = Some(req.action_prefix.clone()).filter(|p| !p.is_empty());
        let query = EffectQuery { since, action_prefix: prefix };
        let (size, token) =
            req.page.as_option().map_or((0, ""), |p| (p.page_size, p.page_token.as_str()));
        let after = if token.is_empty() {
            None
        } else {
            let refused = || error::invalid("page.page_token", "not a token this list gave");
            Some(self.cursors.open_effect(token).ok_or_else(refused)?)
        };
        let listed = self
            .app
            .list_effects(&caller, of, query, Page::new(after, size))
            .await
            .map_err(error::from_app)?;
        let next = listed.next.map(|n| self.cursors.seal_effect(n)).transpose()?;
        Response::ok(pb::ListEffectsResponse {
            effects: listed.items.iter().map(effect_to_pb).collect(),
            page: pb::PageResponse {
                next_page_token: next.unwrap_or_default(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
    }
}
