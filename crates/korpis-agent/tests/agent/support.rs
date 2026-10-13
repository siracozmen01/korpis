// SPDX-License-Identifier: AGPL-3.0-or-later

//! A control plane for the agent to join.

use connectrpc::client::{ClientConfig, HttpClient};
use korpis_core::tenancy::Name;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_recipe::artifact::{Artifact, File};
use korpis_recipe::registry::Registry;
use korpis_server::data::DataDir;
use korpis_server::install::{InstallConfig, install};
use korpis_store::testing::TestDb;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::sync::Arc;
use std::time::Duration;

pub struct ControlPlane {
    db: TestDb,
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
            db,
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

    fn config(&self) -> ClientConfig {
        ClientConfig::new(self.url.parse().unwrap())
            .with_default_header("authorization", format!("Bearer {}", self.secret))
    }

    pub fn nodes(&self) -> pb::NodeServiceClient<HttpClient> {
        pb::NodeServiceClient::new(self.http(), self.config())
    }

    pub fn workloads(&self) -> pb::WorkloadServiceClient<HttpClient> {
        pb::WorkloadServiceClient::new(self.http(), self.config())
    }

    /// A project in the root organization, whose policy runs unsigned recipes: the agent's
    /// tests are about the node, not about trust (§7 of docs/09-recipes.md).
    pub async fn project(&self) -> String {
        let policy = pb::TrustPolicy { allow_unsigned: true, ..Default::default() };
        pb::OrganizationServiceClient::new(self.http(), self.config())
            .set_trust_policy(pb::SetTrustPolicyRequest {
                mutation: mutation("unsigned").into(),
                organization: self.root_org.clone(),
                policy: policy.into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let created = pb::ProjectServiceClient::new(self.http(), self.config())
            .create_project(pb::CreateProjectRequest {
                mutation: mutation("project").into(),
                organization: self.root_org.clone(),
                name: "games".into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        created.project.as_option().unwrap().id.clone()
    }

    /// Declares workload `name` from the example recipe in `project`; its wire id.
    pub async fn declare(&self, project: &str, name: &str) -> String {
        let repo = push_paper().await;
        let declared = self
            .workloads()
            .declare_intent(pb::DeclareIntentRequest {
                mutation: mutation(&format!("declare-{name}")).into(),
                target: Some(Target::Create(Box::new(pb::NewWorkload {
                    project: project.to_owned(),
                    name: name.to_owned(),
                    ..Default::default()
                }))),
                body: pb::IntentBody {
                    recipe: pb::RecipeRef {
                        name: repo,
                        version: "1.21.4".into(),
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        declared.workload.as_option().unwrap().id.clone()
    }

    /// One query against the control plane's database.
    pub async fn scalar<T>(&self, sql: &'static str) -> T
    where
        T: Send + Unpin + for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    {
        let store = self.db.store().await.unwrap();
        sqlx::query_scalar(sql).fetch_one(store.pool()).await.unwrap()
    }

    /// One statement against the control plane's database.
    pub async fn execute(&self, sql: &'static str) {
        let store = self.db.store().await.unwrap();
        sqlx::query(sql).execute(store.pool()).await.unwrap();
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

pub fn mutation(key: &str) -> pb::Mutation {
    pb::Mutation { idempotency_key: key.to_owned(), ..Default::default() }
}

/// The example recipe of §3 of docs/09-recipes.md.
const PAPER: &str = include_str!("../../../korpis-recipe/tests/recipe/paper.yaml");

/// Pushes the example to a repository no other test uses; returns the repository.
pub async fn push_paper() -> String {
    let registry =
        std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned());
    let repo = format!("{registry}/test/paper-{}", uuid::Uuid::now_v7().simple());
    let file = |path: &str, data: &[u8]| File {
        path: path.to_owned(),
        media_type: "application/octet-stream".to_owned(),
        data: data.to_vec(),
    };
    let artifact = Artifact {
        document: PAPER.as_bytes().to_vec(),
        files: vec![
            file("layers/rootfs.tar.zst", b"not really a rootfs"),
            file("templates/server.properties", b"motd={{motd}}\n"),
        ],
    };
    Registry::new(vec![registry]).push(&repo, "1.21.4", &artifact).await.unwrap();
    repo
}

/// What a test node reports: the `oci` driver and the fixtures' capacity.
pub fn facts() -> korpis_agent::session::Facts {
    let node = korpis_store::testing::ready_node(uuid::Uuid::now_v7(), "reported", &[]).unwrap();
    let pb = korpis_server::api::wire::node::node_to_pb(
        &node,
        &korpis_core::tenancy::quota::Bounds::default(),
    );
    korpis_agent::session::Facts {
        drivers: pb.drivers,
        storage_classes: Vec::new(),
        capacity: pb.capacity.into_option().unwrap_or_default(),
        allocatable: pb.allocatable.into_option().unwrap_or_default(),
    }
}
