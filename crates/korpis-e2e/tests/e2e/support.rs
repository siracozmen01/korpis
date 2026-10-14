// SPDX-License-Identifier: AGPL-3.0-or-later

//! A control plane for the node to join, installed into a throwaway database and served on a
//! free port, as the agent's own tests make one, and the thin recipe the node runs.

use connectrpc::client::{ClientConfig, HttpClient};
use korpis_core::tenancy::Name;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_recipe::artifact::Artifact;
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
            denial_window: Duration::from_secs(60),
            event_retention: Duration::from_secs(30 * 86_400),
            relay_role: true,
            proxy_protocol_from: Vec::new(),
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

    /// The CLI's connection, as whoever `secret` authenticates; the root token's when none.
    pub fn cli(&self, secret: Option<&str>) -> korpis_cli::connect::Connection {
        let endpoint = korpis_cli::connect::Endpoint {
            server: self.url.clone(),
            ca: self.data.tls().join("ca.pem"),
            token: secret.unwrap_or(&self.secret).to_owned(),
        };
        korpis_cli::connect::Connection::open(&endpoint).unwrap()
    }

    /// A new API token holding `actions` on the root organization and nothing else, so no
    /// authority over its nodes; its secret.
    pub async fn token_for(&self, actions: &[&str]) -> String {
        use korpis_core::authz::{Kind, ObjectRef};
        use korpis_core::id::{Id, IdKind};
        let org = Id::parse(IdKind::Organization, &self.root_org).unwrap().uuid();
        let scope = korpis_core::scope::Scope::Object(ObjectRef::new(Kind::Organization, org));
        pb::GrantServiceClient::new(self.http(), self.config())
            .issue_grant(pb::IssueGrantRequest {
                mutation: mutation(&actions.join(",")).into(),
                actions: actions.iter().map(|a| (*a).to_owned()).collect(),
                scope: korpis_wire::scope::to_pb(&scope).into(),
                label: "console only".into(),
                to_new_token: true,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned()
            .token
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

    /// Declares workload `name` in `project` from the recipe at `repo`; its wire id.
    pub async fn declare(&self, project: &str, name: &str, repo: &str) -> String {
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
                        name: repo.to_owned(),
                        version: VERSION.into(),
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

    /// What the control plane says of a workload.
    pub async fn status(&self, workload: &str) -> pb::WorkloadStatus {
        let got = self
            .workloads()
            .get_workload(pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() })
            .await
            .unwrap()
            .into_owned();
        got.status.as_option().unwrap().clone()
    }

    /// Deletes a workload and approves its retirement.
    pub async fn retire(&self, workload: &str) {
        let deleted = self
            .workloads()
            .delete_workload(pb::DeleteWorkloadRequest {
                mutation: mutation(&format!("delete-{workload}")).into(),
                workload: workload.to_owned(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        let plan = deleted.plan.as_option().unwrap().id.clone();
        pb::PlanServiceClient::new(self.http(), self.config())
            .approve_plan(pb::ApprovePlanRequest {
                mutation: mutation(&format!("approve-{plan}")).into(),
                plan,
                ..Default::default()
            })
            .await
            .unwrap();
    }

    /// One query against the control plane's database.
    pub async fn scalar<T>(&self, sql: &'static str) -> T
    where
        T: Send + Unpin + for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    {
        let store = self.db.store().await.unwrap();
        sqlx::query_scalar(sql).fetch_one(store.pool()).await.unwrap()
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

/// The image the thin recipe runs, by digest.
pub const BUSYBOX: &str = "docker.io/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e";
/// The thin recipe's version.
pub const VERSION: &str = "1.0.0";

/// A thin recipe (§2 of docs/09-recipes.md): an image by digest, an entry that records each
/// start in its volume, a configured greeting it writes there and prints to its console, and
/// what it finds of its identity socket (§9.5 of docs/08-identity.md).
fn sleeper() -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: sleeper, version: "{VERSION}", description: sleeps, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{BUSYBOX}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - >-
        echo $GREETING > /data/greeting; echo said $GREETING;
        {{ test -S /run/korpis/identity.sock && echo socket;
        touch /run/korpis/x 2>/dev/null || echo read-only; }} > /data/identity;
        date +%s%N >> /data/starts; exec sleep 3600
preset:
  lifecycle: persistent
  console: logs
  volumes:
    - {{name: data, path: /data, size_default: 64Mi}}
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu:    {{min: 100m, recommended: 250m}}
config:
  fields:
    - {{key: greeting, type: string, default: hello, deliver: env, deliver_as: GREETING}}
"#
    )
}

/// Pushes the thin recipe to a repository no other test uses; returns the repository.
pub async fn push_sleeper() -> String {
    let registry =
        std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned());
    let repo = format!("{registry}/test/sleeper-{}", uuid::Uuid::now_v7().simple());
    let artifact = Artifact { document: sleeper().into_bytes(), files: Vec::new() };
    Registry::new(vec![registry]).push(&repo, VERSION, &artifact).await.unwrap();
    repo
}
