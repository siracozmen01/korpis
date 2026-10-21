// SPDX-License-Identifier: AGPL-3.0-or-later

//! A control plane for the node to join, installed into a throwaway database and served on a
//! free port, as the agent's own tests make one.

use connectrpc::client::{ClientConfig, HttpClient};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_server::data::DataDir;
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
        Self::start_serving(acme, None).await
    }

    /// A control plane serving the built panel in `web_root` beside its API, its passkeys bound
    /// to `localhost` and to its own origin, as a browser reaches it.
    #[allow(dead_code, reason = "used by the browser test, a binary of its own")]
    pub async fn start_panel(web_root: std::path::PathBuf) -> Self {
        Self::start_serving(korpis_server::acme::AcmeConfig::default(), Some(web_root)).await
    }

    async fn start_serving(
        acme: korpis_server::acme::AcmeConfig,
        web_root: Option<std::path::PathBuf>,
    ) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        // A panel's origin names its port before the server holds it, so a free one is taken.
        let port = match &web_root {
            Some(_) => {
                std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
            }
            None => 0,
        };
        let db = TestDb::new().await.expect("test database");
        let dir = tempfile::tempdir().expect("tempdir");
        let data = DataDir::new(dir.path().join("data"));
        // Served as installation serves it: by a login that owns nothing.
        let login = db.login().await.expect("login");
        let installed = korpis_server::testing::install_acme(&db.owner_url, &data, &login).await;
        let running = korpis_server::serve(serve_config(login, &data, port, acme, web_root))
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

    /// Runs one statement against the control plane's database, as a test's fixture.
    #[allow(dead_code, reason = "used by the browser test, a binary of its own")]
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

/// How the tests' control plane serves: as `login`, on `port` of the loopback address, a panel from
/// `web_root` with its passkeys bound to `localhost` when one is given.
fn serve_config(
    login: String,
    data: &DataDir,
    port: u16,
    acme: korpis_server::acme::AcmeConfig,
    web_root: Option<std::path::PathBuf>,
) -> korpis_server::ServeConfig {
    let relying_party = web_root.as_ref().map(|_| korpis_core::webauthn::RelyingParty {
        id: "localhost".into(),
        origins: vec![format!("https://localhost:{port}")],
    });
    korpis_server::ServeConfig {
        data: data.clone(),
        database_url: login,
        listen: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        max_connections: 4,
        fetch_allow: Vec::new(),
        plain_http_registries: vec![
            std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned()),
        ],
        placement_policy: korpis_core::schedule::PlacementPolicy::default(),
        placement_retry: Duration::from_millis(200),
        watch_resync: Duration::from_secs(1),
        heartbeat_interval: Duration::from_secs(30),
        // Long enough that a renewal delayed on a loaded machine does not let a lease lapse: a
        // run on a lapsed lease is judged lost (§4.2 of docs/05-scheduling.md), which is right
        // of a node gone silent and wrong of one that was merely slow. Nothing here tests
        // expiry; the agent's and the API's tests do, at terms of their own.
        lease_term: Duration::from_secs(15),
        metering_interval: std::time::Duration::from_secs(3600),
        denial_window: Duration::from_secs(60),
        event_retention: Duration::from_secs(30 * 86_400),
        stream_retention_floor: std::time::Duration::ZERO,
        relay_role: true,
        proxy_protocol_from: Vec::new(),
        dns_resolver: None,
        acme,
        rates: korpis_core::rate::Rates::default(),
        relying_party,
        session_limits: korpis_core::account::session::SessionLimits::default(),
        web_root,
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
    /// Never this machine's keyring: the test's client keeps nothing of its own.
    pub fn cli_env(&self, secret: &str) -> [(&'static str, String); 4] {
        [
            ("KORPIS_SERVER", self.url.clone()),
            ("KORPIS_CA", self.data.tls().join("ca.pem").to_string_lossy().into_owned()),
            ("KORPIS_TOKEN", secret.to_owned()),
            ("KORPIS_KEYRING", "off".to_owned()),
        ]
    }
}

/// Waits until workload `w` has converged.
pub async fn converged(cp: &ControlPlane, w: &str) {
    for _ in 0..900 {
        let s = cp.status(w).await;
        if s.convergence.as_known() == Some(pb::Convergence::CONVERGENCE_CONVERGED) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{w} never converged: {:?}", cp.status(w).await);
}
