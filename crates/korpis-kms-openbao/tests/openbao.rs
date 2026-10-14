// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider against a real OpenBao (`scripts/test-tools.sh`), run as a host process in its
//! development mode; skipped where it is not installed. A data key is made and opened for its own
//! context alone; a key that would ignore the context is refused; the answer the SDK gives the
//! control plane is the key sealed to the call's recipient; and once the tenant revokes the
//! provider's token, nothing opens.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_extension::keys::{KeyManagement, Keys};
use korpis_extension::keyseal::Recipient;
use korpis_extension::provider::{Handler, Outcome};
use korpis_kms_openbao::Transit;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const ROOT: &str = "root";

/// An OpenBao in development mode, stopped when dropped.
struct Bao {
    child: Child,
    address: url::Url,
    http: reqwest::Client,
}

impl Drop for Bao {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn binary() -> Option<PathBuf> {
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.tools/bin/bao");
    bin.exists().then_some(bin)
}

impl Bao {
    async fn start() -> Option<Self> {
        let bin = binary()?;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let child = Command::new(bin)
            .args(["server", "-dev", &format!("-dev-root-token-id={ROOT}")])
            .arg(format!("-dev-listen-address=127.0.0.1:{port}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let address: url::Url = format!("http://127.0.0.1:{port}/").parse().unwrap();
        let bao = Self { child, address, http: reqwest::Client::new() };
        for _ in 0..100 {
            let health = bao.http.get(bao.address.join("v1/sys/health").unwrap()).send().await;
            if health.is_ok_and(|r| r.status().is_success()) {
                return Some(bao);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("OpenBao did not start");
    }

    async fn post(&self, path: &str, token: &str, body: Value) -> Value {
        let r = self
            .http
            .post(self.address.join(&format!("v1/{path}")).unwrap())
            .header("X-Vault-Token", token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{path}: {}", r.text().await.unwrap());
        r.json().await.unwrap_or(Value::Null)
    }

    /// Transit mounted, a derived key and a plain one made, and a token for the provider whose
    /// policy allows the derived key's datakey and decrypt and the reading of keys.
    async fn tenant(&self) -> String {
        self.post("sys/mounts/transit", ROOT, json!({ "type": "transit" })).await;
        self.post("transit/keys/tenant", ROOT, json!({ "derived": true })).await;
        self.post("transit/keys/plain", ROOT, json!({})).await;
        let policy = r#"
            path "transit/datakey/plaintext/*" { capabilities = ["update"] }
            path "transit/decrypt/*" { capabilities = ["update"] }
            path "transit/keys/*" { capabilities = ["read"] }
        "#;
        self.post("sys/policies/acl/korpis", ROOT, json!({ "policy": policy })).await;
        let made = self.post("auth/token/create", ROOT, json!({ "policies": ["korpis"] })).await;
        made["auth"]["client_token"].as_str().unwrap().to_owned()
    }

    fn transit(&self, token: &str) -> Transit {
        Transit::new(&self.address, ("transit", None), token.to_owned(), self.http.clone()).unwrap()
    }
}

#[tokio::test]
async fn a_key_opens_for_its_own_context_alone_and_never_after_revocation() {
    let Some(bao) = Bao::start().await else { return };
    let token = bao.tenant().await;
    let transit = bao.transit(&token);
    let (one, two) = (b"rep_1/wl_1".to_vec(), b"rep_1/wl_2".to_vec());
    let (key, wrapped) = transit.generate("tenant".into(), one.clone()).await.unwrap();
    assert!(String::from_utf8_lossy(&wrapped).starts_with("vault:v1:"), "Transit's own wrapping");
    assert_eq!(transit.unwrap("tenant".into(), one.clone(), wrapped.clone()).await.unwrap(), key);
    let elsewhere = transit.unwrap("tenant".into(), two, wrapped.clone()).await;
    assert!(elsewhere.is_err_and(|f| f.reason == "kms.refused"), "another context opens nothing");
    let plain = transit.generate("plain".into(), one.clone()).await.unwrap_err();
    assert_eq!(plain.reason, "kms.key_not_derived");
    assert_eq!(
        transit.generate("../sys".into(), one.clone()).await.unwrap_err().reason,
        "kms.no_key"
    );

    // What the control plane receives: the key sealed to the call's recipient.
    let recipient = Recipient::new().unwrap();
    let call = pb::ProviderCall {
        request: Some(Request::UnwrapKey(Box::new(pb::UnwrapKey {
            key: "tenant".into(),
            context: one.clone(),
            recipient: recipient.public().to_vec(),
            wrapped: wrapped.clone(),
            ..Default::default()
        }))),
        ..Default::default()
    };
    let Outcome::KeyMaterial(m) = Keys(bao.transit(&token)).call(call).await else { panic!() };
    assert_eq!(recipient.open(&m.sealed, &one).unwrap(), key);

    // Revoked by the tenant: nothing opens.
    bao.post("auth/token/revoke", ROOT, json!({ "token": token })).await;
    let revoked = transit.unwrap("tenant".into(), one, wrapped).await.unwrap_err();
    assert_eq!((revoked.reason.as_str(), revoked.retryable), ("kms.denied", false));
}
