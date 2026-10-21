// SPDX-License-Identifier: AGPL-3.0-or-later

//! A person who signs in through their organization's identity provider (§9.7 of
//! docs/08-identity.md), as §44 of docs/23-walkthroughs.md traces it: the independent issuer of
//! `conformance/oidc` on a loopback port, and `korpis-idp-oidc` installed into the root
//! organization as `corp`, its workload on a real node. The provider itself is served from here,
//! through the identity socket and the relay the node serves that workload, exactly as its own
//! `main` serves it inside the workload: the node's egress rules deny a workload every address of
//! its own host (§7 of docs/07-networking.md), and the issuer is on this one. The issuer serves
//! HTTPS under a test authority the provider alone trusts, since the control plane sends a
//! browser to no identity provider over plain HTTP.

use crate::node::Node;
use crate::recipes::{VERSION, busybox, push};
use crate::support::{ControlPlane, mutation};
use crate::terminal::{Login, logging_in};
use korpis_extension::identity_provider::Identity;
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, IdentitySocket};
use korpis_idp_oidc::Oidc;
use korpis_idp_oidc::config::Settings;
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const CLIENT: &str = "korpis";
const SECRET: &str = "a-client-secret";
/// The provider's name in the organization, which its sign-in link names.
const NAME: &str = "corp";

/// The issuer's process, stopped when dropped.
struct Issuer(Child);

impl Drop for Issuer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The issuer, the provider serving it, and the sign-in link; all stopped when dropped.
struct Corp {
    _issuer: Issuer,
    serving: tokio::task::JoinHandle<()>,
    link: String,
    _dir: tempfile::TempDir,
}

impl Drop for Corp {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

/// Dana's part of the browser tests: the provider she signs in through, and a `korpis login` on
/// her terminal waiting for her to approve it.
pub struct Dana {
    corp: Corp,
    login: Login,
}

/// Dana's part where `node` runs here and the issuer is installed (`scripts/test-tools.sh`),
/// her terminal's sessions under `config`; `None` elsewhere.
pub async fn dana(
    cp: &ControlPlane,
    project: &str,
    node: Option<&Node>,
    korpis: &Path,
    config: &Path,
) -> Option<Dana> {
    let corp = corp(cp, project, node?).await?;
    let login = logging_in(cp, korpis, config).await;
    Some(Dana { corp, login })
}

impl Dana {
    /// Tells the browser tests her sign-in link and where her terminal asks to be approved.
    pub fn env(&self, playwright: &mut tokio::process::Command) {
        playwright.env("KORPIS_E2E_SIGN_IN", &self.corp.link);
        playwright.env("KORPIS_E2E_PERSON_DEVICE_URL", &self.login.at);
    }

    /// Fails the test unless her terminal was signed in as her: its profile is her binding's.
    pub async fn approved(self) {
        let said = self.login.said().await;
        assert!(said.contains("\"dana\""), "signed in as the person who approved it: {said}");
        drop(self.corp);
    }
}

fn package() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/oidc")
}

/// The provider's document: an extension providing identity as `corp`, whose workload only
/// holds its place on the node while the provider is served beside it.
fn document() -> String {
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Extension
metadata: {{name: corp-idp, version: "{VERSION}", description: signs staff in, license: AGPL-3.0-or-later}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
  entry:
    command: [/bin/sh, -c, "trap 'exit 0' TERM; while true; do sleep 1; done"]
preset:
  lifecycle: persistent
  console: none
  resources:
    memory: {{min: 32Mi, recommended: 64Mi}}
    cpu: {{min: 50m, recommended: 100m}}
extension:
  api_version: korpis.extension.v1
  requires: [provider.serve, provider.identity]
  provides:
    - {{type: identity, name: {NAME}}}
"#,
        busybox = busybox(),
    )
}

/// A test authority, in PEM, and a certificate it issued for 127.0.0.1, written to `dir`.
fn issued(dir: &Path) -> String {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let authority = ca.self_signed(&ca_key).unwrap();
    let signer = rcgen::Issuer::new(ca, ca_key);
    let key = rcgen::KeyPair::generate().unwrap();
    let leaf = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    let leaf = leaf.signed_by(&key, &signer).unwrap();
    std::fs::write(dir.join("cert.pem"), leaf.pem()).unwrap();
    std::fs::write(dir.join("key.pem"), key.serialize_pem()).unwrap();
    authority.pem()
}

/// A client trusting only `authority`, following no redirect, as the provider's own is.
fn client(authority: &str) -> reqwest::Client {
    let authority = reqwest::Certificate::from_pem(authority.as_bytes()).unwrap();
    reqwest::Client::builder()
        .tls_certs_only([authority])
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// Starts the issuer for the panel at `panel`, knowing only Dana, in `ops` and `oncall`, over
/// HTTPS; it, its address, and a client that trusts it.
async fn issuer(panel: &str, dir: &Path) -> (Issuer, String, reqwest::Client) {
    let http = client(&issued(dir));
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let people = dir.join("people.json");
    let known = serde_json::json!({"next": "dana", "people": {"dana": ["ops", "oncall"]}});
    std::fs::write(&people, known.to_string()).unwrap();
    let child = Issuer(
        Command::new("node")
            .arg(package().join("issuer.mjs"))
            .env("PORT", port.to_string())
            .env("PEOPLE", &people)
            .envs([("CLIENT_ID", CLIENT), ("CLIENT_SECRET", SECRET)])
            .env("REDIRECT_URI", format!("{panel}/sign-in/callback"))
            .env("TLS_CERT", dir.join("cert.pem"))
            .env("TLS_KEY", dir.join("key.pem"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let url = format!("https://127.0.0.1:{port}");
    let discovery = format!("{url}/.well-known/openid-configuration");
    for _ in 0..100 {
        if http.get(&discovery).send().await.is_ok_and(|r| r.status().is_success()) {
            return (child, url, http);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the issuer did not start");
}

/// Installs the provider into the root organization, its workload in `project`; the workload.
async fn install(cp: &ControlPlane, project: &str) -> String {
    let repo = push("corp-idp", document()).await;
    let client = pb::ExtensionServiceClient::new(cp.http(), cp.config());
    let request = pb::InstallExtensionRequest {
        mutation: mutation("corp").into(),
        artifact: repo,
        version: VERSION.into(),
        organization: cp.root_org.clone(),
        workload_project: project.to_owned(),
        ..Default::default()
    };
    let done = client.install_extension(request).await.unwrap().into_owned();
    done.extension.into_option().unwrap().workload
}

/// The issuer, and `korpis-idp-oidc` installed as `corp` and registered through `node`, which
/// runs its workload; `None` where the issuer is not installed.
async fn corp(cp: &ControlPlane, project: &str, node: &Node) -> Option<Corp> {
    if !package().join("node_modules/oidc-provider").exists() {
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let (issuer, url, http) = issuer(&cp.url, dir.path()).await;
    let workload = install(cp, project).await;
    node.guard(&workload);
    let socket = node.state.root().join("identity").join(&workload).join("identity.sock");
    for _ in 0..600 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let settings = Settings {
        issuer: url,
        client_id: CLIENT.into(),
        client_secret: SECRET.into(),
        groups_claim: "groups".into(),
        scopes: vec!["openid".into(), "groups".into()],
        state_dir: None,
    };
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_IDENTITY,
        name: NAME.into(),
        version: VERSION.into(),
        concurrency: 4,
        sign_in: true,
    };
    let connection = Arc::new(Connection::new(IdentitySocket::at(socket)));
    let provider =
        Arc::new(Provider::new(connection, registration, Identity(Oidc::new(http, settings))));
    let mut registered = provider.registered();
    let serving = tokio::spawn(async move {
        let _ = provider.run().await;
    });
    tokio::time::timeout(Duration::from_secs(60), registered.wait_for(Option::is_some))
        .await
        .expect("the provider registered within a minute")
        .unwrap();
    let link = format!("/sign-in/{}/{NAME}", cp.root_org);
    Some(Corp { _issuer: issuer, serving, link, _dir: dir })
}
