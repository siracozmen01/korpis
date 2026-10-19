// SPDX-License-Identifier: AGPL-3.0-or-later

//! A control plane for the agent to join.

use connectrpc::client::{ClientConfig, HttpClient};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_server::data::DataDir;
use korpis_store::testing::TestDb;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::sync::Arc;
use std::time::Duration;

mod authority;
mod recipe;
mod relay;
mod served;

pub use recipe::{push_paper, push_paper_waiting};

pub struct ControlPlane {
    db: TestDb,
    _dir: tempfile::TempDir,
    pub data: DataDir,
    pub url: String,
    pub root_org: String,
    secret: String,
    login: String,
    relay_role: bool,
    served: served::Served,
}

impl ControlPlane {
    pub async fn start() -> Self {
        Self::start_with(true).await
    }

    /// A control plane that serves the relay role itself only if `relay_role`.
    pub async fn start_with(relay_role: bool) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let db = TestDb::new().await.expect("test database");
        let dir = tempfile::tempdir().expect("tempdir");
        let data = DataDir::new(dir.path().join("data"));
        // Served as installation serves it: by a login that owns nothing.
        let login = db.login().await.expect("login");
        let installed = korpis_server::testing::install_acme(&db.owner_url, &data, &login).await;
        let listen = "127.0.0.1:0".parse().unwrap();
        let served = served::Served::start(served::config(&data, &login, listen, relay_role)).await;
        let root_org = korpis_server::api::wire::wire_id(
            korpis_core::id::IdKind::Organization,
            installed.organization.id,
        );
        Self {
            db,
            _dir: dir,
            data,
            url: format!("https://localhost:{}", served.addr.port()),
            root_org,
            secret: installed.token().unwrap().1.expose().to_owned(),
            login,
            relay_role,
            served,
        }
    }

    /// Stops serving, as stopping the process does: every task it started ends.
    pub fn stop(&mut self) {
        self.served.stop();
    }

    /// The database as it is now, copied, as a backup of it would restore; serving must have
    /// stopped. Its login.
    pub async fn snapshot(&self) -> (TestDb, String) {
        let copy = self.db.copy().await.expect("copy");
        let login = copy.login().await.expect("login");
        (copy, login)
    }

    /// The login the control plane serves its database by.
    pub fn login(&self) -> &str {
        &self.login
    }

    /// Serves again, on the same address, from `db` by `login`: the same database, or a copy of
    /// an older state of it.
    pub async fn serve_on(&mut self, db: Option<(TestDb, String)>) {
        self.served.stop();
        if let Some((db, login)) = db {
            self.db = db;
            self.login = login;
        }
        let listen = self.served.addr;
        let config = served::config(&self.data, &self.login, listen, self.relay_role);
        self.served = served::Served::start(config).await;
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

    /// The workload service as whoever `secret` authenticates.
    pub fn workloads_as(&self, secret: &str) -> pb::WorkloadServiceClient<HttpClient> {
        let config = ClientConfig::new(self.url.parse().unwrap())
            .with_default_header("authorization", format!("Bearer {secret}"));
        pb::WorkloadServiceClient::new(self.http(), config)
    }

    /// A client of `S` as the operator.
    pub fn client<S>(&self, make: impl FnOnce(HttpClient, ClientConfig) -> S) -> S {
        make(self.http(), self.config())
    }

    /// The identifier of the volume `name` in `project`, once its node has reported it.
    pub async fn volume_named(&self, project: &str, name: &str) -> String {
        let client = pb::VolumeServiceClient::new(self.http(), self.config());
        for _ in 0..200 {
            let request =
                pb::ListVolumesRequest { project: project.to_owned(), ..Default::default() };
            let listed = client.list_volumes(request).await.unwrap().into_owned().volumes;
            if let Some(v) = listed.into_iter().find(|v| v.name == name) {
                return v.id;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("no volume {name} was reported");
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
        self.declare_with(project, name, Vec::new()).await
    }

    /// Declares workload `name` as [`Self::declare`] does, with `volumes` declared.
    pub async fn declare_with(
        &self,
        project: &str,
        name: &str,
        volumes: Vec<pb::VolumeSpec>,
    ) -> String {
        // The recipe's game endpoint is `stable`, which without an edge tier needs a node
        // reachable on its own address (§3 of docs/07-networking.md). The test nodes advertise
        // the loopback address, which the control plane's prober reaches; wait until it has.
        self.measured().await;
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
                    volumes,
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

    /// Cancels a plan.
    pub async fn cancel(&self, plan: &str) {
        pb::PlanServiceClient::new(self.http(), self.config())
            .cancel_plan(pb::CancelPlanRequest {
                mutation: mutation(&format!("cancel-{plan}")).into(),
                plan: plan.to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
    }

    /// One plan.
    pub async fn plan(&self, plan: &str) -> pb::Plan {
        let got = pb::PlanServiceClient::new(self.http(), self.config())
            .get_plan(pb::GetPlanRequest { id: plan.to_owned(), ..Default::default() })
            .await
            .unwrap()
            .into_owned();
        got.plan.into_option().unwrap()
    }

    /// One query against the control plane's database.
    pub async fn scalar<T>(&self, sql: &'static str) -> T
    where
        T: Send + Unpin + for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    {
        let store = self.db.store().await.unwrap();
        sqlx::query_scalar(sql).fetch_one(store.pool()).await.unwrap()
    }

    /// One row, with one text parameter, from the control plane's database, if there is one.
    pub async fn query_one<T>(&self, sql: &'static str, arg: &str) -> Option<T>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> + Send + Unpin,
    {
        let store = self.db.store().await.unwrap();
        sqlx::query_as(sql).bind(arg.to_owned()).fetch_optional(store.pool()).await.unwrap()
    }

    /// Waits until every connected node's reachability has been measured (§9 of
    /// docs/07-networking.md).
    pub async fn measured(&self) {
        for _ in 0..300 {
            let waiting: i64 = self
                .scalar("SELECT count(*) FROM node WHERE connectivity = 'connected' AND probed_at IS NULL")
                .await;
            if waiting == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the nodes were not measured within thirty seconds");
    }

    /// One statement against the control plane's database.
    pub async fn execute(&self, sql: &'static str) {
        let store = self.db.store().await.unwrap();
        sqlx::query(sql).execute(store.pool()).await.unwrap();
    }

    /// One statement as the database's owner, as a restore writes it: what the application role
    /// may not do, such as removing effects.
    pub async fn execute_as_owner(&self, sql: &'static str) {
        use sqlx::Connection as _;
        let mut owner = sqlx::PgConnection::connect(&self.db.owner_url).await.unwrap();
        sqlx::query(sql).execute(&mut owner).await.unwrap();
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
        // The loopback address, declared as an operator would declare a LAN address: what the
        // reachability probe of a test control plane on this machine can reach.
        advertised: korpis_proto::korpis::agent::v1::Advertised {
            addresses: vec!["127.0.0.1".into()],
            first_port: 30_000,
            last_port: 32_767,
            ..Default::default()
        },
    }
}
