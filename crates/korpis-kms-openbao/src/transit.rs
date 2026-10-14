// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tenant's Transit engine as a key management service (§5.5 of docs/06-storage.md). A data
//! key is made by `datakey/plaintext`, which returns it and its wrapping, and opened by `decrypt`,
//! each with the call's context, so the engine derives a key per context and a wrapping opens for
//! its own `<repository>/<workload>` alone. A key that is not `derived` would ignore the context,
//! and a credential taken from the provider would then open every workload's keys, so such a key
//! is refused. The engine's own policy decides who may call it, and its audit device logs each.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use korpis_extension::keys::{DataKey, Failure, KeyManagement};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};
use zeroize::Zeroizing;

/// A Transit engine, at its mount, reached with the provider's own token.
#[derive(Debug)]
pub struct Transit {
    /// `<address>/v1/<mount>/`.
    base: url::Url,
    token: Zeroizing<String>,
    namespace: Option<String>,
    http: reqwest::Client,
    /// Keys found derived, which need not be asked again.
    derived: Mutex<BTreeSet<String>>,
}

fn failure(reason: &str, message: impl Into<String>, retryable: bool) -> Failure {
    Failure { reason: reason.into(), message: message.into(), retryable }
}

/// Whether `name` is a Transit key name that stays one path segment.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && name != "."
        && name != ".."
}

impl Transit {
    /// The engine at `address`, mounted at `mount`, called with `token`, in `namespace` if set.
    ///
    /// # Errors
    /// The mount is not one path segment, or the address cannot take it.
    pub fn new(
        address: &url::Url,
        (mount, namespace): (&str, Option<String>),
        token: String,
        http: reqwest::Client,
    ) -> Result<Self, String> {
        if !plain(mount) {
            return Err(format!("{mount:?} is not a mount"));
        }
        let base = address.join(&format!("v1/{mount}/")).map_err(|e| e.to_string())?;
        let token = Zeroizing::new(token);
        Ok(Self { base, token, namespace, http, derived: Mutex::new(BTreeSet::new()) })
    }

    /// Calls `path` beneath the mount, with `body` or none; the answer's `data`.
    async fn call(&self, path: &str, body: Option<Value>) -> Result<Value, Failure> {
        let url = self.base.join(path).map_err(|e| failure("kms.refused", e.to_string(), false))?;
        let mut request = match &body {
            Some(b) => self.http.post(url).json(b),
            None => self.http.get(url),
        };
        request = request.header("X-Vault-Token", self.token.as_str());
        if let Some(ns) = &self.namespace {
            request = request.header("X-Vault-Namespace", ns);
        }
        let answer =
            request.send().await.map_err(|e| failure("kms.unreachable", e.to_string(), true))?;
        let status = answer.status();
        let text =
            answer.text().await.map_err(|e| failure("kms.unreachable", e.to_string(), true))?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(value.get("data").cloned().unwrap_or(Value::Null));
        }
        let said = value.get("errors").map(ToString::to_string).unwrap_or_default();
        Err(match status.as_u16() {
            401 | 403 => failure("kms.denied", said, false),
            404 => failure("kms.no_key", said, false),
            429 | 500..=599 => failure("kms.unreachable", said, true),
            _ => failure("kms.refused", said, false),
        })
    }

    /// That `key` exists and derives a key per context; asked once per key.
    async fn derived(&self, key: &str) -> Result<(), Failure> {
        if !plain(key) {
            return Err(failure("kms.no_key", format!("{key:?} is not a key name"), false));
        }
        if self.derived.lock().unwrap_or_else(PoisonError::into_inner).contains(key) {
            return Ok(());
        }
        let data = self.call(&format!("keys/{key}"), None).await?;
        if data.get("derived").and_then(Value::as_bool) != Some(true) {
            let why = "the key is not derived, so it would open every context's keys";
            return Err(failure("kms.key_not_derived", why, false));
        }
        self.derived.lock().unwrap_or_else(PoisonError::into_inner).insert(key.to_owned());
        Ok(())
    }
}

/// A 32-byte key from the base64 field `field` of `data`.
fn key_in(data: &Value, field: &str) -> Result<DataKey, Failure> {
    let malformed = || failure("kms.malformed", format!("no 32-byte {field}"), false);
    let text = data.get(field).and_then(Value::as_str).ok_or_else(malformed)?;
    let bytes = Zeroizing::new(STANDARD.decode(text).map_err(|_| malformed())?);
    bytes.as_slice().try_into().map_err(|_| malformed())
}

impl KeyManagement for Transit {
    async fn generate(&self, key: String, context: Vec<u8>) -> Result<(DataKey, Vec<u8>), Failure> {
        self.derived(&key).await?;
        let body = json!({ "context": STANDARD.encode(&context), "bits": 256 });
        let data = self.call(&format!("datakey/plaintext/{key}"), Some(body)).await?;
        let made = key_in(&data, "plaintext")?;
        let wrapped = data.get("ciphertext").and_then(Value::as_str);
        let wrapped = wrapped.ok_or_else(|| failure("kms.malformed", "no ciphertext", false))?;
        Ok((made, wrapped.as_bytes().to_vec()))
    }

    async fn unwrap(
        &self,
        key: String,
        context: Vec<u8>,
        wrapped: Vec<u8>,
    ) -> Result<DataKey, Failure> {
        self.derived(&key).await?;
        let ciphertext = String::from_utf8(wrapped)
            .map_err(|_| failure("kms.refused", "not a Transit ciphertext", false))?;
        let body = json!({ "ciphertext": ciphertext, "context": STANDARD.encode(&context) });
        let data = self.call(&format!("decrypt/{key}"), Some(body)).await?;
        key_in(&data, "plaintext")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_one_path_segment_and_nothing_else() {
        assert!(plain("tenant-key_1.v2"));
        for bad in ["", ".", "..", "a/b", "a?b", "a%2fb", &"k".repeat(129)] {
            assert!(!plain(bad), "{bad:?}");
        }
    }
}
