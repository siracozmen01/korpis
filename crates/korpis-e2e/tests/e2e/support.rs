// SPDX-License-Identifier: AGPL-3.0-or-later

//! A control plane for the node to join, installed into a throwaway database and served on a
//! free port, as the agent's own tests make one.

use connectrpc::client::{ClientConfig, HttpClient};
use korpis_core::tenancy::Name;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
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
        Self::start_with(korpis_server::acme::AcmeConfig::default()).await
    }

    /// A control plane ordering certificates from the ACME directory `acme` names.
    pub async fn start_with(acme: korpis_server::acme::AcmeConfig) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let db = TestDb::new().await.expect("test database");
        let dir = tempfile::tempdir().expect("tempdir");
        let data = DataDir::new(dir.path().join("data"));
        let installed = install(&InstallConfig {
            data: data.clone(),
            owner_url: db.owner_url.clone(),
            organization: Name::parse("acme").unwrap(),
            tls_names: vec!["localhost".to_owned()],
            root: korpis_server::install::RootHolder::Token,
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
            placement_retry: Duration::from_millis(200),
            watch_resync: Duration::from_secs(1),
            heartbeat_interval: Duration::from_secs(30),
            lease_term: Duration::from_secs(2),
            denial_window: Duration::from_secs(60),
            event_retention: Duration::from_secs(30 * 86_400),
            stream_retention_floor: std::time::Duration::ZERO,
            relay_role: true,
            proxy_protocol_from: Vec::new(),
            dns_resolver: None,
            acme,
            rates: korpis_core::rate::Rates::default(),
            relying_party: None,
            session_limits: korpis_core::account::session::SessionLimits::default(),
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
            secret: installed.token().unwrap().1.expose().to_owned(),
            server: running.task,
        }
    }

    pub(crate) fn http(&self) -> HttpClient {
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

    pub(crate) fn config(&self) -> ClientConfig {
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
        self.declare_with(project, name, repo, Vec::new()).await
    }

    /// As [`Self::declare`], with `volumes` declared over the recipe's.
    pub async fn declare_with(
        &self,
        project: &str,
        name: &str,
        repo: &str,
        volumes: Vec<pb::VolumeSpec>,
    ) -> String {
        self.declare_body(project, name, repo, pb::IntentBody { volumes, ..Default::default() })
            .await
    }

    /// As [`Self::declare`], with `body` declared over the recipe's; its recipe is `repo`'s.
    pub async fn declare_body(
        &self,
        project: &str,
        name: &str,
        repo: &str,
        body: pb::IntentBody,
    ) -> String {
        let recipe = pb::RecipeRef {
            name: repo.to_owned(),
            version: crate::recipes::VERSION.into(),
            ..Default::default()
        };
        let request = pb::DeclareIntentRequest {
            mutation: mutation(&format!("declare-{name}")).into(),
            target: Some(Target::Create(Box::new(pb::NewWorkload {
                project: project.to_owned(),
                name: name.to_owned(),
                ..Default::default()
            }))),
            body: pb::IntentBody { recipe: recipe.into(), ..body }.into(),
            ..Default::default()
        };
        let declared =
            Box::pin(self.workloads().declare_intent(request)).await.unwrap().into_owned();
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
        self.approve(&plan).await;
    }

    /// The identifier of `project`'s live volume called `name`.
    pub async fn volume_named(&self, project: &str, name: &str) -> String {
        let request = pb::ListVolumesRequest { project: project.to_owned(), ..Default::default() };
        let listed = pb::VolumeServiceClient::new(self.http(), self.config())
            .list_volumes(request)
            .await
            .unwrap()
            .into_owned()
            .volumes;
        listed.into_iter().find(|v| v.name == name).unwrap().id
    }

    /// Approves `plan` as the operator.
    pub async fn approve(&self, plan: &str) {
        pb::PlanServiceClient::new(self.http(), self.config())
            .approve_plan(pb::ApprovePlanRequest {
                mutation: mutation(&format!("approve-{plan}")).into(),
                plan: plan.to_owned(),
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

/// The `korpis` binary, built where this test was if it is not already.
pub fn korpis() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(std::path::Path::parent).unwrap();
    let target = profile_dir.parent().unwrap();
    let profile = match profile_dir.file_name().unwrap().to_str().unwrap() {
        "debug" => "dev",
        other => other,
    };
    let built = std::process::Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", "korpis-cli", "--bin", "korpis"])
        .args(["--profile", profile, "--target-dir"])
        .arg(target)
        .status()
        .unwrap();
    assert!(built.success(), "korpis did not build");
    profile_dir.join("korpis")
}

impl ControlPlane {
    /// What the CLI is told to reach this control plane as whoever `secret` authenticates.
    pub fn cli_env(&self, secret: &str) -> [(&'static str, String); 3] {
        [
            ("KORPIS_SERVER", self.url.clone()),
            ("KORPIS_CA", self.data.tls().join("ca.pem").to_string_lossy().into_owned()),
            ("KORPIS_TOKEN", secret.to_owned()),
        ]
    }
}
