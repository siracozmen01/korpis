// SPDX-License-Identifier: AGPL-3.0-or-later

//! A control plane for the agent to join.

use connectrpc::client::{ClientConfig, HttpClient};
use korpis_core::tenancy::Name;
use korpis_proto::korpis::v1 as pb;
use korpis_server::data::DataDir;
use korpis_server::install::{InstallConfig, install};
use korpis_store::testing::TestDb;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::sync::Arc;
use std::time::Duration;

pub struct ControlPlane {
    _db: TestDb,
    _dir: tempfile::TempDir,
    pub data: DataDir,
    pub url: String,
    pub root_org: String,
    secret: String,
    server: tokio::task::JoinHandle<Result<(), korpis_server::ServeError>>,
}

impl Drop for ControlPlane {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl ControlPlane {
    pub async fn start() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let db = TestDb::new().await.expect("test database");
        let dir = tempfile::tempdir().expect("tempdir");
        let data = DataDir::new(dir.path().join("data"));
        let installed = install(&InstallConfig {
            data: data.clone(),
            owner_url: db.owner_url.clone(),
            organization: Name::parse("acme").unwrap(),
            tls_names: vec!["localhost".to_owned()],
        })
        .await
        .expect("install");
        let running = korpis_server::serve(korpis_server::ServeConfig {
            data: data.clone(),
            database_url: db.owner_url.clone(),
            listen: "127.0.0.1:0".parse().unwrap(),
            max_connections: 4,
            plain_http_registries: vec![
                std::env::var("KORPIS_TEST_REGISTRY")
                    .unwrap_or_else(|_| "127.0.0.1:55000".to_owned()),
            ],
            placement_policy: korpis_core::schedule::PlacementPolicy::default(),
            placement_retry: Duration::from_millis(200),
            watch_resync: Duration::from_secs(1),
            heartbeat_interval: Duration::from_secs(30),
            lease_term: Duration::from_secs(2),
        })
        .await
        .expect("serve");
        let root_org = korpis_server::api::wire::wire_id(
            korpis_core::id::IdKind::Organization,
            installed.organization.id,
        );
        Self {
            _db: db,
            _dir: dir,
            data,
            url: format!("https://localhost:{}", running.addr.port()),
            root_org,
            secret: installed.secret.expose().to_owned(),
            server: running.task,
        }
    }

    fn http(&self) -> HttpClient {
        let ca = CertificateDer::from_pem_file(self.data.tls().join("ca.pem")).expect("CA");
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca).expect("CA accepted");
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        HttpClient::with_tls(Arc::new(tls))
    }

    pub fn nodes(&self) -> pb::NodeServiceClient<HttpClient> {
        let config = ClientConfig::new(self.url.parse().unwrap())
            .with_default_header("authorization", format!("Bearer {}", self.secret));
        pb::NodeServiceClient::new(self.http(), config)
    }

    /// A one-time token for a node named `name`; the token and the CA fingerprint.
    pub async fn token(&self, name: &str) -> (String, String) {
        let created = self
            .nodes()
            .create_enrollment_token(pb::CreateEnrollmentTokenRequest {
                mutation: pb::Mutation {
                    idempotency_key: format!("t-{name}"),
                    ..Default::default()
                }
                .into(),
                organization: self.root_org.clone(),
                node_name: name.to_owned(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        (created.token, created.ca_fingerprint)
    }
}
