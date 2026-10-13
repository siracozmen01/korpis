// SPDX-License-Identifier: AGPL-3.0-or-later

//! `DeclaredSetService` (§3.3 of docs/01-model.md, §5 and §6 of docs/13-surface-cli.md).

use super::wire::workload::{declared_set_to_pb, drift_mode_from_pb};
use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, next_page, page, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::workload::SetDriftMode;
use korpis_core::id::IdKind;
use korpis_core::workload::set::DeclaredSet;
use korpis_proto::korpis::v1 as pb;

fn mode_set(s: &DeclaredSet) -> pb::SetDeclaredSetModeResponse {
    pb::SetDeclaredSetModeResponse { set: declared_set_to_pb(s).into(), ..Default::default() }
}

impl pb::DeclaredSetService for Api {
    async fn list_declared_sets(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListDeclaredSetsRequest>,
    ) -> ServiceResult<pb::ListDeclaredSetsResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let page = page(req.page.as_option(), IdKind::Set)?;
        let project = wire::id(IdKind::Project, "project", &req.project).refuse()?;
        let listed =
            self.app.list_declared_sets(&caller, project, page).await.map_err(error::from_app)?;
        Response::ok(pb::ListDeclaredSetsResponse {
            sets: listed.items.iter().map(declared_set_to_pb).collect(),
            page: next_page(listed.next, IdKind::Set).into(),
            ..Default::default()
        })
    }

    async fn set_declared_set_mode(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetDeclaredSetModeRequest>,
    ) -> ServiceResult<pb::SetDeclaredSetModeResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let mutation = req.mutation.as_option();
        let key = mutation_key(mutation)?;
        let cmd = SetDriftMode {
            project: wire::id(IdKind::Project, "project", &req.project).refuse()?,
            name: wire::name("name", &req.name).refuse()?,
            mode: drift_mode_from_pb("mode", req.mode).refuse()?,
            expected_version: mutation
                .and_then(|m| m.expected_version)
                .ok_or_else(|| error::invalid("mutation.expected_version", "the set's version"))?,
        };
        let encoder = |s: &DeclaredSet| encode(&mode_set(s));
        let idem = Idempotent { key, encode: &encoder };
        let reply = self.app.set_drift_mode(&caller, &idem, cmd).await.map_err(error::from_app)?;
        respond(reply, mode_set)
    }
}
