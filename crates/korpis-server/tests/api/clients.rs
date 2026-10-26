// SPDX-License-Identifier: AGPL-3.0-or-later

//! A client of each service of the control plane the harness runs, called with `config`.

use crate::support::Harness;
use connectrpc::client::{ClientConfig, HttpClient};
use korpis_proto::korpis::v1 as pb;

impl Harness {
    pub fn orgs(&self, config: ClientConfig) -> pb::OrganizationServiceClient<HttpClient> {
        pb::OrganizationServiceClient::new(self.http.clone(), config)
    }

    pub fn projects(&self, config: ClientConfig) -> pb::ProjectServiceClient<HttpClient> {
        pb::ProjectServiceClient::new(self.http.clone(), config)
    }

    pub fn workloads(&self, config: ClientConfig) -> pb::WorkloadServiceClient<HttpClient> {
        pb::WorkloadServiceClient::new(self.http.clone(), config)
    }

    pub fn plans(&self, config: ClientConfig) -> pb::PlanServiceClient<HttpClient> {
        pb::PlanServiceClient::new(self.http.clone(), config)
    }

    pub fn grants(&self, config: ClientConfig) -> pb::GrantServiceClient<HttpClient> {
        pb::GrantServiceClient::new(self.http.clone(), config)
    }

    pub fn identity(&self, config: ClientConfig) -> pb::IdentityServiceClient<HttpClient> {
        pb::IdentityServiceClient::new(self.http.clone(), config)
    }

    pub fn streams(&self, config: ClientConfig) -> pb::StreamServiceClient<HttpClient> {
        pb::StreamServiceClient::new(self.http.clone(), config)
    }

    pub fn volumes(&self, config: ClientConfig) -> pb::VolumeServiceClient<HttpClient> {
        pb::VolumeServiceClient::new(self.http.clone(), config)
    }

    pub fn domains(&self, config: ClientConfig) -> pb::DomainServiceClient<HttpClient> {
        pb::DomainServiceClient::new(self.http.clone(), config)
    }

    pub fn certificates(&self, config: ClientConfig) -> pb::CertificateServiceClient<HttpClient> {
        pb::CertificateServiceClient::new(self.http.clone(), config)
    }

    pub fn portals(&self, config: ClientConfig) -> pb::PortalServiceClient<HttpClient> {
        pb::PortalServiceClient::new(self.http.clone(), config)
    }

    pub fn repositories(&self, config: ClientConfig) -> pb::RepositoryServiceClient<HttpClient> {
        pb::RepositoryServiceClient::new(self.http.clone(), config)
    }

    pub fn backups(&self, config: ClientConfig) -> pb::BackupServiceClient<HttpClient> {
        pb::BackupServiceClient::new(self.http.clone(), config)
    }

    pub fn runs(&self, config: ClientConfig) -> pb::RunServiceClient<HttpClient> {
        pb::RunServiceClient::new(self.http.clone(), config)
    }

    pub fn sets(&self, config: ClientConfig) -> pb::DeclaredSetServiceClient<HttpClient> {
        pb::DeclaredSetServiceClient::new(self.http.clone(), config)
    }

    pub fn events(&self, config: ClientConfig) -> pb::EventServiceClient<HttpClient> {
        pb::EventServiceClient::new(self.http.clone(), config)
    }

    pub fn extensions(&self, config: ClientConfig) -> pb::ExtensionServiceClient<HttpClient> {
        pb::ExtensionServiceClient::new(self.http.clone(), config)
    }

    pub fn metering(&self, config: ClientConfig) -> pb::MeteringServiceClient<HttpClient> {
        pb::MeteringServiceClient::new(self.http.clone(), config)
    }

    pub fn effect_log(&self, config: ClientConfig) -> pb::EffectServiceClient<HttpClient> {
        pb::EffectServiceClient::new(self.http.clone(), config)
    }

    pub fn pools(&self, config: ClientConfig) -> pb::AddressPoolServiceClient<HttpClient> {
        pb::AddressPoolServiceClient::new(self.http.clone(), config)
    }

    pub fn nodes(&self, config: ClientConfig) -> pb::NodeServiceClient<HttpClient> {
        pb::NodeServiceClient::new(self.http.clone(), config)
    }
}
