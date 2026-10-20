// SPDX-License-Identifier: AGPL-3.0-or-later

//! The independent issuer (`conformance/oidc`), run as a host process on a loopback port, and a
//! browser's part of a sign-in through it: following the issuer's redirects, cookies kept, until
//! it sends the person back to the panel's callback with a code.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use korpis_extension::identity_provider::{IdentityProvider, SignInAsked};
use korpis_idp_oidc::Oidc;
use korpis_idp_oidc::config::Settings;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub const CLIENT: &str = "korpis";
pub const SECRET: &str = "a-client-secret";
pub const CALLBACK: &str = "http://127.0.0.1:9/sign-in/callback";

/// The issuer, stopped when dropped.
pub struct Issuer {
    child: Child,
    pub url: String,
    people: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for Issuer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn package() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/oidc")
}

impl Issuer {
    /// The issuer signing its ID tokens with `alg`, or `None` where it is not installed
    /// (`scripts/test-tools.sh`).
    pub async fn start(alg: &str) -> Option<Self> {
        if !package().join("node_modules/oidc-provider").exists() {
            // Said where the check reads it (scripts/check.sh); a test only passes.
            return None;
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let people = dir.path().join("people.json");
        let issuer = Self {
            child: Command::new("node")
                .arg(package().join("issuer.mjs"))
                .envs([("PORT", port.to_string()), ("ALG", alg.to_owned())])
                .env("PEOPLE", &people)
                .envs([
                    ("CLIENT_ID", CLIENT),
                    ("CLIENT_SECRET", SECRET),
                    ("REDIRECT_URI", CALLBACK),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
            url: format!("http://127.0.0.1:{port}"),
            people,
            _dir: dir,
        };
        issuer.set("dana", &[("dana", &["ops", "oncall"])]);
        let discovery = format!("{}/.well-known/openid-configuration", issuer.url);
        for _ in 0..100 {
            if reqwest::get(&discovery).await.is_ok_and(|r| r.status().is_success()) {
                return Some(issuer);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the issuer did not start");
    }

    /// Who the issuer knows, with their groups, and who signs in next.
    pub fn set(&self, next: &str, people: &[(&str, &[&str])]) {
        let people: BTreeMap<&str, &[&str]> = people.iter().copied().collect();
        std::fs::write(&self.people, json!({ "next": next, "people": people }).to_string())
            .unwrap();
    }

    /// The provider's settings for this issuer, keeping refresh tokens under `state` if given.
    pub fn settings(&self, state: Option<&Path>) -> Settings {
        let mut scopes = vec!["openid".to_owned(), "groups".to_owned()];
        if state.is_some() {
            scopes.push("offline_access".into());
        }
        Settings {
            issuer: self.url.clone(),
            client_id: CLIENT.into(),
            client_secret: SECRET.into(),
            groups_claim: "groups".into(),
            scopes,
            state_dir: state.map(Path::to_owned),
        }
    }
}

/// A provider for `settings`, as the extension runs it.
pub fn provider(settings: Settings) -> Oidc {
    let http =
        reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    Oidc::new(http, settings)
}

/// What the panel and the control plane hold of one sign-in, and the code it came back with.
pub struct Returned {
    pub code: String,
    pub verifier: String,
    pub nonce: String,
}

fn random() -> String {
    let mut bytes = [0_u8; 32];
    SystemRandom::new().fill(&mut bytes).unwrap();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A sign-in through `oidc`, begun as the control plane begins one and followed as a browser
/// would, to the code the issuer sends the person back with.
pub async fn sign_in(oidc: &Oidc) -> Returned {
    let (state, nonce, verifier) = (random(), random(), random());
    let challenge = URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()));
    let asked = SignInAsked {
        state: state.clone(),
        nonce: nonce.clone(),
        code_challenge: challenge,
        redirect_uri: CALLBACK.into(),
    };
    let mut at: url::Url = oidc.authorize(asked).await.unwrap().parse().unwrap();
    let browser = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    for _ in 0..12 {
        let answer = browser.get(at.clone()).send().await.unwrap();
        let location = answer.headers().get("location").expect("a redirect").to_str().unwrap();
        at = at.join(location).unwrap();
        if at.as_str().starts_with(CALLBACK) {
            let query: BTreeMap<_, _> = at.query_pairs().into_owned().collect();
            assert_eq!(query.get("state"), Some(&state), "the state comes back as it went");
            return Returned { code: query["code"].clone(), verifier, nonce };
        }
    }
    panic!("the issuer never sent the person back");
}

/// The headers the control plane asserts a returned code with (§9.7 of docs/08-identity.md).
pub fn headers(r: &Returned) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("code_verifier".to_owned(), r.verifier.clone()),
        ("redirect_uri".to_owned(), CALLBACK.to_owned()),
        ("nonce".to_owned(), r.nonce.clone()),
    ])
}
