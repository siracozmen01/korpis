// SPDX-License-Identifier: AGPL-3.0-or-later

//! The object store a job reaches with the access a backup target granted it (§5.6 of
//! docs/06-storage.md): an S3 bucket, beneath the place in it the granted prefix has. Each
//! request is presigned for a few minutes with the job's temporary credential and sent over
//! HTTPS; one that fails for a reason that passes (the network, the store being busy or briefly
//! down) is tried again, a little later each time, and one the store refuses is not.

use crate::store::{ObjectStore, StoreError};
use rusty_s3::actions::{ListObjectsV2, S3Action as _};
use rusty_s3::{Bucket, Credentials, UrlStyle};
use std::time::Duration;

/// How long a presigned request is valid: long enough to send a chunk on a slow link.
const SIGNED_FOR: Duration = Duration::from_secs(300);
/// How many times a request that failed for a passing reason is sent.
const ATTEMPTS: u32 = 5;
/// The most keys one listing page holds, which is S3's own limit.
const PAGE: usize = 1000;

/// The access a job was granted: where the store is and the credential to use there.
#[derive(Clone)]
pub struct S3Config {
    /// The service's address, `https://host[:port]`.
    pub endpoint: String,
    /// Its region.
    pub region: String,
    /// The bucket.
    pub bucket: String,
    /// Where the granted prefix is in the bucket; every object is beneath it.
    pub key_prefix: String,
    /// The access key, its secret, and the session token of a temporary credential.
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
    /// Whether the bucket is addressed in the path rather than the host name.
    pub path_style: bool,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("key_prefix", &self.key_prefix)
            .finish_non_exhaustive()
    }
}

/// A bucket, beneath one prefix.
#[derive(Debug)]
pub struct S3Store {
    bucket: Bucket,
    credentials: Credentials,
    prefix: String,
    http: reqwest::Client,
}

fn refused(what: impl std::fmt::Display) -> StoreError {
    StoreError::Refused(what.to_string())
}

impl S3Store {
    /// The store `config` names. Nothing is sent until it is used.
    pub fn new(config: S3Config) -> Result<Self, StoreError> {
        if !config.key_prefix.is_empty() && !config.key_prefix.ends_with('/') {
            return Err(refused("a key prefix ends with '/'"));
        }
        let endpoint = url::Url::parse(&config.endpoint).map_err(refused)?;
        if endpoint.scheme() == "http" && !loopback(&endpoint) {
            return Err(refused("a store on another machine is reached over HTTPS"));
        }
        let style = if config.path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
        let bucket = Bucket::new(endpoint, style, config.bucket, config.region).map_err(refused)?;
        let credentials = if config.session_token.is_empty() {
            Credentials::new(config.access_key_id, config.secret_access_key)
        } else {
            Credentials::new_with_token(
                config.access_key_id,
                config.secret_access_key,
                config.session_token,
            )
        };
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| StoreError::Failed(e.to_string()))?;
        Ok(Self { bucket, credentials, prefix: config.key_prefix, http })
    }

    /// The bucket's key for `name`, once `name` is known to stay beneath the prefix.
    fn key(&self, name: &str) -> Result<String, StoreError> {
        let ok =
            !name.is_empty() && name.split('/').all(|s| !s.is_empty() && s != "." && s != "..");
        if !ok {
            return Err(refused(format!("{name:?} is not an object name")));
        }
        Ok(format!("{}{name}", self.prefix))
    }

    /// Sends the request `build` makes until it is answered for good: a response, or a failure
    /// that will not pass.
    async fn send(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, StoreError> {
        let mut wait = Duration::from_millis(200);
        let mut last = String::new();
        for _ in 0..ATTEMPTS {
            match build().send().await {
                Ok(r) if !passing(r.status()) => return Ok(r),
                Ok(r) => last = format!("{} from the store", r.status()),
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(wait).await;
            wait = wait.saturating_mul(2);
        }
        Err(StoreError::Failed(last))
    }
}

/// Whether `url` names this machine, the one place a request may travel unencrypted.
fn loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(a)) => a.is_loopback(),
        Some(url::Host::Ipv6(a)) => a.is_loopback(),
        Some(url::Host::Domain(d)) => d == "localhost",
        None => false,
    }
}

/// Whether a status says the store may answer differently soon.
fn passing(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
}

/// The failure a response that was not a success stands for.
async fn failure(r: reqwest::Response) -> StoreError {
    let status = r.status();
    let body = r.text().await.unwrap_or_default();
    // S3 answers with an XML error whose code says why; the code is enough to act on.
    let code = body.split("<Code>").nth(1).and_then(|s| s.split("</Code>").next()).unwrap_or("");
    refused(format!("{status} {code}"))
}

impl ObjectStore for S3Store {
    async fn put(&self, name: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        let url =
            self.bucket.put_object(Some(&self.credentials), &self.key(name)?).sign(SIGNED_FOR);
        let body = bytes::Bytes::from(bytes);
        let r = self.send(|| self.http.put(url.clone()).body(body.clone())).await?;
        if r.status().is_success() { Ok(()) } else { Err(failure(r).await) }
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let url =
            self.bucket.get_object(Some(&self.credentials), &self.key(name)?).sign(SIGNED_FOR);
        let r = self.send(|| self.http.get(url.clone())).await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !r.status().is_success() {
            return Err(failure(r).await);
        }
        let body = r.bytes().await.map_err(|e| StoreError::Failed(e.to_string()))?;
        Ok(Some(body.to_vec()))
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let full = format!("{}{prefix}", self.prefix);
        let mut names = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
            action.with_prefix(full.as_str());
            action.with_max_keys(PAGE);
            if let Some(t) = &token {
                action.with_continuation_token(t.as_str());
            }
            let url = action.sign(SIGNED_FOR);
            let r = self.send(|| self.http.get(url.clone())).await?;
            if !r.status().is_success() {
                return Err(failure(r).await);
            }
            let text = r.text().await.map_err(|e| StoreError::Failed(e.to_string()))?;
            let page = ListObjectsV2::parse_response(&text)
                .map_err(|e| StoreError::Failed(format!("a listing: {e}")))?;
            for o in page.contents {
                // A store that lists outside what was asked is not believed.
                let name = o.key.strip_prefix(&self.prefix).filter(|n| n.starts_with(prefix));
                names.push(
                    name.ok_or_else(|| refused(format!("the store listed {}", o.key)))?.to_owned(),
                );
            }
            match page.next_continuation_token {
                Some(t) if !t.is_empty() => token = Some(t),
                _ => break,
            }
        }
        names.sort();
        Ok(names)
    }
}
