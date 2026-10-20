// SPDX-License-Identifier: AGPL-3.0-or-later

//! The issuer, over HTTP (§5.3 of docs/08-identity.md): its discovery document, whose `issuer`
//! must be the one configured, kept for an hour; its keys, fetched again when a token names one
//! not held, at most once a minute; and its token endpoint, where a code is redeemed and a
//! refresh token used. Every request has the HTTP client's deadline, and no answer is read past
//! a megabyte.

use crate::config::{Settings, secure};
use crate::jwk::{self, Key};
use crate::jws;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;
use url::Url;

const DISCOVERY_KEPT: Duration = Duration::from_secs(3600);
const KEYS_AGAIN: Duration = Duration::from_secs(60);
const MAX_BODY: usize = 1 << 20;

/// Why the issuer gave no answer to believe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// It answered, and what it answered refuses: an `invalid_grant`, a token that does not
    /// verify. Asking again will not help.
    Refused(String),
    /// It could not be reached or answered nothing usable; asking again may help.
    Unreachable(String),
}

#[derive(Debug, Clone)]
struct Endpoints {
    authorization: Url,
    token: Url,
    jwks: Url,
    /// The client authenticates with HTTP Basic, the default; otherwise in the form.
    basic: bool,
}

/// A grant presented at the token endpoint.
#[derive(Debug, Clone, Copy)]
pub enum Grant<'a> {
    /// A code a person returned with, its PKCE verifier, and the redirect URI it was asked for.
    Code { code: &'a str, verifier: &'a str, redirect_uri: &'a str },
    /// A refresh token kept from an earlier answer.
    Refresh(&'a str),
}

/// What the token endpoint answered.
#[derive(Debug, Clone, Default)]
pub struct Tokens {
    /// The ID token, which a refresh may leave out.
    pub id_token: Option<String>,
    /// A refresh token, new or rotated.
    pub refresh_token: Option<String>,
}

/// The configured issuer.
#[derive(Debug)]
pub struct Issuer {
    http: reqwest::Client,
    settings: Settings,
    endpoints: Mutex<Option<(Endpoints, Instant)>>,
    keys: Mutex<Option<(Vec<Key>, Instant)>>,
}

fn unreachable(what: &str, e: impl std::fmt::Display) -> Refusal {
    Refusal::Unreachable(format!("{what}: {e}"))
}

async fn json(what: &str, mut response: reqwest::Response) -> Result<Value, Refusal> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| unreachable(what, e))? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            return Err(Refusal::Unreachable(format!("{what}: the answer is too large")));
        }
    }
    serde_json::from_slice(&body).map_err(|_| Refusal::Unreachable(format!("{what}: not JSON")))
}

fn endpoint(doc: &Value, field: &str) -> Result<Url, Refusal> {
    let url: Url = doc
        .get(field)
        .and_then(Value::as_str)
        .and_then(|u| u.parse().ok())
        .ok_or_else(|| Refusal::Unreachable(format!("discovery names no {field}")))?;
    if secure(&url) { Ok(url) } else { Err(Refusal::Unreachable(format!("{field} is not https"))) }
}

impl Issuer {
    /// The issuer `settings` names, reached with `http`.
    #[must_use]
    pub fn new(http: reqwest::Client, settings: Settings) -> Self {
        Self { http, settings, endpoints: Mutex::new(None), keys: Mutex::new(None) }
    }

    /// What it is configured with.
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    async fn get(&self, what: &str, url: Url) -> Result<Value, Refusal> {
        let response = self.http.get(url).send().await.map_err(|e| unreachable(what, e))?;
        if !response.status().is_success() {
            return Err(unreachable(what, response.status()));
        }
        json(what, response).await
    }

    async fn endpoints(&self) -> Result<Endpoints, Refusal> {
        let mut held = self.endpoints.lock().await;
        if let Some((e, at)) = held.as_ref()
            && at.elapsed() < DISCOVERY_KEPT
        {
            return Ok(e.clone());
        }
        let base = self.settings.issuer.trim_end_matches('/');
        let url = format!("{base}/.well-known/openid-configuration");
        let url = url.parse().map_err(|e| unreachable("discovery", e))?;
        let doc = self.get("discovery", url).await?;
        // OpenID Connect Discovery §4.3: the issuer it names is the one asked, exactly.
        if doc.get("issuer").and_then(Value::as_str) != Some(self.settings.issuer.as_str()) {
            return Err(Refusal::Unreachable("discovery names another issuer".into()));
        }
        let methods = doc.get("token_endpoint_auth_methods_supported").and_then(Value::as_array);
        let basic = methods.is_none_or(|m| m.iter().any(|v| v == "client_secret_basic"));
        let found = Endpoints {
            authorization: endpoint(&doc, "authorization_endpoint")?,
            token: endpoint(&doc, "token_endpoint")?,
            jwks: endpoint(&doc, "jwks_uri")?,
            basic,
        };
        *held = Some((found.clone(), Instant::now()));
        Ok(found)
    }

    /// The issuer's authorization endpoint.
    ///
    /// # Errors
    /// Its discovery document cannot be read.
    pub async fn authorization_endpoint(&self) -> Result<Url, Refusal> {
        Ok(self.endpoints().await?.authorization)
    }

    async fn keys(&self, again: bool) -> Result<Vec<Key>, Refusal> {
        let mut held = self.keys.lock().await;
        if let Some((k, at)) = held.as_ref()
            && (!again || at.elapsed() < KEYS_AGAIN)
        {
            return Ok(k.clone());
        }
        let url = self.endpoints().await?.jwks;
        let keys = jwk::keys(&self.get("keys", url).await?);
        *held = Some((keys.clone(), Instant::now()));
        Ok(keys)
    }

    /// The claims of `token`, once a key the issuer publishes verifies it.
    ///
    /// # Errors
    /// It does not verify, or the keys cannot be read.
    pub async fn verified(&self, token: &str) -> Result<Value, Refusal> {
        let signed = jws::read(token).map_err(Refusal::Refused)?;
        let mut keys = self.keys(false).await?;
        if !signed.verified_by(&keys) && !signed.names_one_of(&keys) {
            keys = self.keys(true).await?;
        }
        if signed.verified_by(&keys) {
            Ok(signed.claims().clone())
        } else {
            Err(Refusal::Refused("the ID token is not signed by the issuer".into()))
        }
    }

    /// Presents `grant` at the token endpoint.
    ///
    /// # Errors
    /// The endpoint refused it, or could not be reached.
    pub async fn redeem(&self, grant: Grant<'_>) -> Result<Tokens, Refusal> {
        let endpoints = self.endpoints().await?;
        let s = &self.settings;
        let mut form: Vec<(&str, &str)> = match grant {
            Grant::Code { code, verifier, redirect_uri } => vec![
                ("grant_type", "authorization_code"),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect_uri),
            ],
            Grant::Refresh(token) => {
                vec![("grant_type", "refresh_token"), ("refresh_token", token)]
            }
        };
        let mut request = self.http.post(endpoints.token);
        if endpoints.basic {
            let encode =
                |v: &str| url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>();
            // RFC 6749 §2.3.1: each form-encoded before they are joined.
            let pair = format!("{}:{}", encode(&s.client_id), encode(&s.client_secret));
            request = request.header("authorization", format!("Basic {}", STANDARD.encode(pair)));
        } else {
            form.extend([("client_id", s.client_id.as_str()), ("client_secret", &s.client_secret)]);
        }
        let response = request.form(&form).send().await.map_err(|e| unreachable("token", e))?;
        let status = response.status();
        let body = json("token", response).await?;
        if status.is_client_error() {
            let error = body.get("error").and_then(Value::as_str).unwrap_or("refused");
            return Err(Refusal::Refused(format!("the issuer refused the grant: {error}")));
        }
        if !status.is_success() {
            return Err(unreachable("token", status));
        }
        let text = |f: &str| body.get(f).and_then(Value::as_str).map(str::to_owned);
        Ok(Tokens { id_token: text("id_token"), refresh_token: text("refresh_token") })
    }
}
