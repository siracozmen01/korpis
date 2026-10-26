// SPDX-License-Identifier: AGPL-3.0-or-later

//! Every service the server answers, on one router.

use super::Api;
use connectrpc::Router;
use korpis_proto::korpis::v1::{
    AccountServiceExt, AddressPoolServiceExt, BackupServiceExt, CertificateServiceExt,
    DeclaredSetServiceExt, DomainServiceExt, EdgeGroupServiceExt, EffectServiceExt,
    EventServiceExt, ExtensionServiceExt, GrantServiceExt, IdentityServiceExt, MeteringServiceExt,
    NodeServiceExt, OrganizationServiceExt, PlanServiceExt, PortalServiceExt, ProfileServiceExt,
    ProjectServiceExt, ProviderServiceExt, RepositoryServiceExt, RunServiceExt,
    ServerInfoServiceExt, SessionServiceExt, StreamServiceExt, VolumeServiceExt,
    WorkloadServiceExt,
};
use std::sync::Arc;

impl Api {
    /// Registers every client service, and the agent service, on a router.
    #[must_use]
    pub fn router(self: Arc<Self>) -> Router {
        let router = Router::new();
        let router = ServerInfoServiceExt::register(Arc::clone(&self), router);
        let router = OrganizationServiceExt::register(Arc::clone(&self), router);
        let router = ProjectServiceExt::register(Arc::clone(&self), router);
        let router = GrantServiceExt::register(Arc::clone(&self), router);
        let router = IdentityServiceExt::register(Arc::clone(&self), router);
        let router = WorkloadServiceExt::register(Arc::clone(&self), router);
        let router = PlanServiceExt::register(Arc::clone(&self), router);
        let router = NodeServiceExt::register(Arc::clone(&self), router);
        let router = EffectServiceExt::register(Arc::clone(&self), router);
        let router = MeteringServiceExt::register(Arc::clone(&self), router);
        let router = DeclaredSetServiceExt::register(Arc::clone(&self), router);
        let router = ExtensionServiceExt::register(Arc::clone(&self), router);
        let router = EventServiceExt::register(Arc::clone(&self), router);
        let router = ProviderServiceExt::register(Arc::clone(&self), router);
        let router = StreamServiceExt::register(Arc::clone(&self), router);
        let router = VolumeServiceExt::register(Arc::clone(&self), router);
        let router = RepositoryServiceExt::register(Arc::clone(&self), router);
        let router = DomainServiceExt::register(Arc::clone(&self), router);
        let router = CertificateServiceExt::register(Arc::clone(&self), router);
        let router = PortalServiceExt::register(Arc::clone(&self), router);
        let router = AddressPoolServiceExt::register(Arc::clone(&self), router);
        let router = EdgeGroupServiceExt::register(Arc::clone(&self), router);
        let router = BackupServiceExt::register(Arc::clone(&self), router);
        let router = RunServiceExt::register(Arc::clone(&self), router);
        let router = AccountServiceExt::register(Arc::clone(&self), router);
        let router = SessionServiceExt::register(Arc::clone(&self), router);
        let router = ProfileServiceExt::register(Arc::clone(&self), router);
        let router =
            if self.serves_relay { Arc::clone(&self.relay).register(router) } else { router };
        let router = korpis_proto::korpis::relay::v1::RelayControlServiceExt::register(
            Arc::clone(&self),
            router,
        );
        korpis_proto::korpis::agent::v1::AgentServiceExt::register(self, router)
    }
}
