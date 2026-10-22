// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ServerInfoService`: what this control plane is, the source it was built from (§2 of
//! docs/19-governance.md), and where sessions open (§3.1 of docs/02-architecture.md).
//! Unauthenticated: it reveals nothing a client could not learn by connecting, since relays serve
//! the clients that reach them, and a client needs it before it has credentials. The sign-in
//! page's theme is what that page shows anyone who loads it (§7 of docs/11-surface-web.md); at a
//! portal, both name the portal and its organization, which the connection proved.

use super::Api;
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_core::build;
use korpis_core::extension::{CAPABILITIES, CONTRACT_VERSIONS};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::wire_id;

use korpis_app::node::AGENT_PROTOCOLS;

impl pb::ServerInfoService for Api {
    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn get_server_info(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetServerInfoRequest>,
    ) -> ServiceResult<pb::GetServerInfoResponse> {
        let relays = self.named_relays.subscribe().borrow().as_ref().map(|r| r.urls.clone());
        let at = super::account::at(&ctx);
        Response::ok(pb::GetServerInfoResponse {
            version: build::VERSION.to_owned(),
            source_revision: build::SOURCE_REVISION.to_owned(),
            source_url: build::SOURCE_URL.to_owned(),
            capabilities: CAPABILITIES.iter().map(|c| (*c).to_owned()).collect(),
            agent_protocols: AGENT_PROTOCOLS.iter().map(|p| (*p).to_owned()).collect(),
            extension_contracts: CONTRACT_VERSIONS.iter().map(|v| (*v).to_owned()).collect(),
            relays: relays.unwrap_or_default(),
            control_plane_relay: self.serves_relay,
            portal: at.map(|p| wire_id(IdKind::Portal, p.portal)).unwrap_or_default(),
            portal_organization: at
                .map(|p| wire_id(IdKind::Organization, p.organization))
                .unwrap_or_default(),
            sftp: self.sftp.listed(),
            ..Default::default()
        })
    }

    async fn get_sign_in_theme(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetSignInThemeRequest>,
    ) -> ServiceResult<pb::GetSignInThemeResponse> {
        super::theme::sign_in(self, super::account::at(&ctx).map(|p| p.portal)).await
    }
}
