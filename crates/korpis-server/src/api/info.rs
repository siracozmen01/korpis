// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ServerInfoService`: what this control plane is, and the source it was built from (§2 of
//! docs/19-governance.md). Unauthenticated: it reveals nothing a client could not learn by
//! connecting, and a client needs it before it has credentials.

use super::Api;
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_core::build;
use korpis_core::extension::{CAPABILITIES, CONTRACT_VERSIONS};
use korpis_proto::korpis::v1 as pb;

use korpis_app::node::AGENT_PROTOCOLS;

impl pb::ServerInfoService for Api {
    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn get_server_info(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetServerInfoRequest>,
    ) -> ServiceResult<pb::GetServerInfoResponse> {
        Response::ok(pb::GetServerInfoResponse {
            version: build::VERSION.to_owned(),
            source_revision: build::SOURCE_REVISION.to_owned(),
            source_url: build::SOURCE_URL.to_owned(),
            capabilities: CAPABILITIES.iter().map(|c| (*c).to_owned()).collect(),
            agent_protocols: AGENT_PROTOCOLS.iter().map(|p| (*p).to_owned()).collect(),
            extension_contracts: CONTRACT_VERSIONS.iter().map(|v| (*v).to_owned()).collect(),
            ..Default::default()
        })
    }
}
