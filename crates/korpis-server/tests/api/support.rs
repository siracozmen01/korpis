// SPDX-License-Identifier: AGPL-3.0-or-later

//! A control plane installed into a throwaway database and served on a free port.

use buffa::Message;
use connectrpc::ConnectError;
use connectrpc::client::{ClientConfig, HttpClient};
use korpis_app::install::Installation;
use korpis_core::authz::credential::Secret;
use korpis_core::authz::{Kind, ObjectRef, Scope};
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_core::tenancy::Name;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::{principal, scope, wire_id};
use korpis_server::data::DataDir;
use korpis_server::install::{InstallConfig, install};
use korpis_store::testing::TestDb;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::sync::Arc;
use uuid::Uuid;

/// The signer of every test recipe [`crate::recipes::push_paper`] pushes.
pub const TEST_SIGNER: &str = "korpis-test";

/// The key behind [`TEST_SIGNER`].
pub fn test_key() -> p256::ecdsa::SigningKey {
    p256::ecdsa::SigningKey::from_slice(&[7; 32]).expect("a valid scalar")
}

/// A signer for a policy, by name, of `key`.
pub fn signer_of(name: &str, key: &p256::ecdsa::SigningKey) -> pb::Signer {
    pb::Signer {
        name: name.to_owned(),
        public_key_pem: korpis_core::recipe::trust::cosign::public_pem(key).expect("PEM"),
        ..Default::default()
    }
}

/// [`TEST_SIGNER`] as a policy names it.
pub fn test_signer() -> pb::Signer {
    signer_of(TEST_SIGNER, &test_key())
}

pub struct Harness {
    pub db: TestDb,
    _dir: tempfile::TempDir,
    pub installed: Installation,
    pub http: HttpClient,
    base: http::Uri,
    /// The installation's CA, PEM, as an extension's node would hand it over.
    pub ca_pem: Vec<u8>,
    /// The control plane's state, to call use cases no RPC reaches yet.
    pub api: std::sync::Arc<korpis_server::api::Api>,
    server: tokio::task::JoinHandle<Result<(), korpis_server::ServeError>>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn client_tls(data: &DataDir) -> HttpClient {
    let ca = CertificateDer::from_pem_file(data.tls().join("ca.pem")).expect("CA");
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

impl Harness {
    pub async fn new() -> Self {
        Self::with_heartbeat(std::time::Duration::from_secs(30)).await
    }

    /// A control plane whose agents send heartbeats every `heartbeat`. The default is long, so
    /// that the nodes tests put in place without an agent are not swept as silent.
    pub async fn with_heartbeat(heartbeat: std::time::Duration) -> Self {
        // What the control plane logs at error, an internal error's cause among it, is printed
        // with the output of the test that failed, so a failure that needs load to happen
        // explains itself.
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error"));
        let _ = tracing_subscriber::fmt().with_env_filter(filter).with_test_writer().try_init();
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
            fetch_allow: Vec::new(),
            plain_http_registries: vec![
                std::env::var("KORPIS_TEST_REGISTRY")
                    .unwrap_or_else(|_| "127.0.0.1:55000".to_owned()),
            ],
            placement_policy: korpis_core::schedule::PlacementPolicy::default(),
            placement_retry: std::time::Duration::from_millis(200),
            watch_resync: std::time::Duration::from_secs(1),
            heartbeat_interval: heartbeat,
            lease_term: std::time::Duration::from_secs(2),
            denial_window: std::time::Duration::from_secs(1),
            event_retention: std::time::Duration::from_secs(30 * 86_400),
            stream_retention_floor: std::time::Duration::ZERO,
            relay_role: true,
            proxy_protocol_from: Vec::new(),
        })
        .await
        .expect("serve");
        let base = format!("https://localhost:{}", running.addr.port()).parse().unwrap();
        let http = client_tls(&data);
        let ca_pem = std::fs::read(data.tls().join("ca.pem")).expect("CA");
        let api = running.api;
        let h = Self { db, _dir: dir, installed, http, base, ca_pem, api, server: running.task };
        h.accept_test_signer().await;
        h
    }

    /// The root organization accepts recipes signed by [`test_key`], as an operator's first act
    /// is to say whose recipes it runs (§7 of docs/09-recipes.md).
    async fn accept_test_signer(&self) {
        let policy =
            pb::TrustPolicy { accepted_signers: vec![test_signer()], ..Default::default() };
        let request = pb::SetTrustPolicyRequest {
            mutation: mutation("accept-test-signer").into(),
            organization: self.root_org(),
            policy: policy.into(),
            ..Default::default()
        };
        self.orgs(self.root()).set_trust_policy(request).await.expect("trust policy");
    }

    /// The control plane's address.
    pub fn base_url(&self) -> String {
        self.base.to_string().trim_end_matches('/').to_owned()
    }

    pub fn root_org(&self) -> String {
        wire_id(IdKind::Organization, self.installed.organization.id)
    }

    pub fn root_grant(&self) -> String {
        wire_id(IdKind::Grant, self.installed.root_grant)
    }

    pub fn config(&self, secret: Option<&str>) -> ClientConfig {
        let config = ClientConfig::new(self.base.clone());
        match secret {
            Some(s) => config.with_default_header("authorization", format!("Bearer {s}")),
            None => config,
        }
    }

    pub fn root(&self) -> ClientConfig {
        self.config(Some(self.installed.secret.expose()))
    }

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

    pub fn streams(&self, config: ClientConfig) -> pb::StreamServiceClient<HttpClient> {
        pb::StreamServiceClient::new(self.http.clone(), config)
    }

    pub fn volumes(&self, config: ClientConfig) -> pb::VolumeServiceClient<HttpClient> {
        pb::VolumeServiceClient::new(self.http.clone(), config)
    }

    pub fn repositories(&self, config: ClientConfig) -> pb::RepositoryServiceClient<HttpClient> {
        pb::RepositoryServiceClient::new(self.http.clone(), config)
    }

    pub fn backups(&self, config: ClientConfig) -> pb::BackupServiceClient<HttpClient> {
        pb::BackupServiceClient::new(self.http.clone(), config)
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

    pub fn effect_log(&self, config: ClientConfig) -> pb::EffectServiceClient<HttpClient> {
        pb::EffectServiceClient::new(self.http.clone(), config)
    }

    pub fn nodes(&self, config: ClientConfig) -> pb::NodeServiceClient<HttpClient> {
        pb::NodeServiceClient::new(self.http.clone(), config)
    }

    /// Enrolls a ready node owned by `organization` (the root organization when `None`), as
    /// the agent protocol will; its wire id.
    pub async fn node(
        &self,
        organization: Option<Uuid>,
        name: &str,
        labels: &[(&str, &str)],
    ) -> String {
        let org = organization.unwrap_or(self.installed.organization.id);
        self.enroll(korpis_store::testing::ready_node(org, name, labels).unwrap()).await
    }

    /// Enrolls `node` as given; its wire id.
    pub async fn enroll(&self, node: korpis_core::node::Node) -> String {
        let store = self.db.store().await.unwrap();
        korpis_store::testing::enroll(&store, &node).await.unwrap();
        wire_id(IdKind::Node, node.id)
    }

    /// An API token with no grants, created directly: Phase 0 has no token-creation call.
    pub async fn bare_token(&self) -> (Uuid, Secret) {
        let store = self.db.store().await.unwrap();
        let id = Uuid::now_v7();
        let secret = Secret::generate().unwrap();
        sqlx::query(
            "INSERT INTO api_token (id, name, secret_hash, created_by)
             VALUES ($1, 'test', $2, 'system:bootstrap')",
        )
        .bind(id)
        .bind(secret.digest().as_slice())
        .execute(store.pool())
        .await
        .unwrap();
        (id, secret)
    }

    /// Issues `actions` on the root organization to `token`, from the root grant; the new grant's id.
    pub async fn issue_to(&self, token: Uuid, key: &str, actions: &[&str]) -> String {
        let issued = self
            .grants(self.root())
            .issue_grant(pb::IssueGrantRequest {
                mutation: mutation(key).into(),
                parent: self.root_grant(),
                subject: principal::to_pb(&Principal::Token(token)).into(),
                actions: actions.iter().map(|a| (*a).to_owned()).collect(),
                scope: org_scope(&self.root_org()).into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        issued.grant.as_option().unwrap().id.clone()
    }

    /// Effect rows for an action, straight from the log.
    pub async fn effects(&self, action: &str) -> Vec<(String, Option<Uuid>)> {
        let store = self.db.store().await.unwrap();
        sqlx::query_as::<_, (String, Option<Uuid>)>(
            "SELECT outcome, grant_id FROM effect WHERE action = $1 ORDER BY at, id",
        )
        .bind(action)
        .fetch_all(store.pool())
        .await
        .unwrap()
    }
}

pub fn mutation(key: &str) -> pb::Mutation {
    pb::Mutation { idempotency_key: key.to_owned(), ..Default::default() }
}

pub fn org_scope(id: &str) -> pb::Scope {
    let id = Id::parse(IdKind::Organization, id).unwrap().uuid();
    scope::to_pb(&Scope::Object(ObjectRef::new(Kind::Organization, id)))
}

/// The structured error detail of §8 of docs/10-api.md.
pub fn detail(e: &ConnectError) -> pb::Error {
    use base64::Engine;
    e.details
        .iter()
        .filter(|d| d.type_url.ends_with("korpis.v1.Error"))
        .filter_map(|d| d.value.as_deref())
        .filter_map(|v| {
            base64::engine::general_purpose::STANDARD_NO_PAD.decode(v.trim_end_matches('=')).ok()
        })
        .find_map(|bytes| pb::Error::decode_from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// The structured reason a client branches on (§8 of docs/10-api.md).
pub fn reason(e: &ConnectError) -> String {
    detail(e).reason
}
