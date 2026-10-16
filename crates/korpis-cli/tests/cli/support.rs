// SPDX-License-Identifier: AGPL-3.0-or-later

//! A control plane for the CLI to call, and the CLI run as a process against it.

use korpis_core::tenancy::Name;
use korpis_server::data::DataDir;
use korpis_server::install::{InstallConfig, install};
use korpis_store::testing::TestDb;
use std::process::{Command, Output};
use std::time::Duration;

/// The development registry (`scripts/dev-registry.sh up`).
pub fn registry() -> String {
    std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned())
}

/// A thin recipe with one configured field, pushed to a repository no other test uses; the
/// repository.
pub async fn push_recipe() -> String {
    let repo = format!("{}/test/cli-{}", registry(), uuid::Uuid::now_v7().simple());
    let document = r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {name: sleeper, version: "1.0.0", description: sleeps, license: MIT}
runtime:
  targets:
    - {tier: container, arch: [amd64, arm64], image: "docker.io/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"}
  entry: {command: [/bin/sh, -c, "exec sleep 3600"]}
preset:
  console: none
config:
  fields:
    - {key: greeting, type: string, default: hello, deliver: env, deliver_as: GREETING}
"#;
    let artifact = korpis_recipe::artifact::Artifact {
        document: document.as_bytes().to_vec(),
        files: Vec::new(),
    };
    korpis_recipe::registry::Registry::new(vec![registry()])
        .unwrap()
        .push(&repo, "1.0.0", &artifact)
        .await
        .unwrap();
    repo
}

pub struct ControlPlane {
    _db: TestDb,
    _dir: tempfile::TempDir,
    pub data: DataDir,
    pub url: String,
    pub root_org: String,
    pub secret: String,
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
            plain_http_registries: vec![registry()],
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
            acme: korpis_server::acme::AcmeConfig::default(),
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
            _db: db,
            _dir: dir,
            data,
            url: format!("https://localhost:{}", running.addr.port()),
            root_org,
            secret: installed.token().unwrap().1.expose().to_owned(),
            server: running.task,
        }
    }

    /// A project named `name` in the root organization, whose policy runs unsigned recipes.
    pub async fn project(&self, name: &str) {
        let policy = format!(
            r#"{{"organization": "{}", "policy": {{"allowUnsigned": true}}}}"#,
            self.root_org
        );
        let set = self
            .korpis(&self.secret, &["call", "OrganizationService/SetTrustPolicy", "-d", &policy])
            .await;
        assert!(set.status.success(), "{}", String::from_utf8_lossy(&set.stderr));
        let project = format!(r#"{{"organization": "{}", "name": "{name}"}}"#, self.root_org);
        let made = self
            .korpis(&self.secret, &["call", "ProjectService/CreateProject", "-d", &project])
            .await;
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    }

    /// `korpis` with `args`, pointed at this control plane with `token`, run to its end.
    pub async fn korpis(&self, token: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_korpis"));
        cmd.args(args)
            .env("KORPIS_SERVER", &self.url)
            .env("KORPIS_CA", self.data.tls().join("ca.pem"))
            .env("KORPIS_TOKEN", token)
            .arg("--plain-http")
            .arg(registry());
        tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap()
    }
}
