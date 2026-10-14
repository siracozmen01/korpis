// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider: access from the token service, scoped by a session policy to the prefix asked
//! for beneath the provider's own place in its bucket, and the listing and deletion a prune asks
//! for, made with the provider's own credential. Keys travel relative to that place, as the
//! control plane addresses them.

use crate::policy::{Mode, is_plain_prefix, session_policy};
use crate::sts::{Sts, StsError};
use korpis_extension::backup::{BackupTarget, Failure};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::object_access::Protocol;
use rusty_s3::actions::{DeleteObjectsResponse, ListObjectsV2, ObjectIdentifier, S3Action as _};
use rusty_s3::{Bucket, Credentials, UrlStyle};
use std::time::{Duration, SystemTime};

/// How long a signed request to the store stays valid.
const SIGNED_FOR: Duration = Duration::from_secs(300);

/// The shortest and longest access the token service issues.
const SHORTEST: Duration = Duration::from_secs(900);
const LONGEST: Duration = Duration::from_secs(12 * 3600);

/// Where the provider keeps backups, and its own credential there.
#[derive(Clone)]
pub struct Place {
    /// The store's address.
    pub endpoint: url::Url,
    /// Its region.
    pub region: String,
    /// The bucket.
    pub bucket: String,
    /// The provider's place in it: empty, or a plain prefix ending in `/`.
    pub root: String,
    /// Whether the bucket is addressed in the path.
    pub path_style: bool,
    /// The provider's own access key.
    pub access_key_id: String,
    /// Its secret.
    pub secret: String,
}

impl std::fmt::Debug for Place {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Place")
            .field("endpoint", &self.endpoint.as_str())
            .field("bucket", &self.bucket)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// The provider.
#[derive(Debug)]
pub struct S3Target {
    place: Place,
    sts: Sts,
    bucket: Bucket,
    credentials: Credentials,
    http: reqwest::Client,
}

fn refused(reason: &str, message: impl std::fmt::Display) -> Failure {
    Failure { reason: reason.into(), message: message.to_string(), retryable: false }
}

fn failed(message: impl std::fmt::Display) -> Failure {
    Failure { reason: "store.failed".into(), message: message.to_string(), retryable: true }
}

fn timestamp(t: SystemTime) -> buffa_types::google::protobuf::Timestamp {
    let since = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    buffa_types::google::protobuf::Timestamp {
        seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(since.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    }
}

/// Whether `key` is an object name a prune may delete: relative, with no empty, `.`, or `..`
/// component.
fn is_object_name(key: &str) -> bool {
    !key.is_empty() && key.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

impl S3Target {
    /// The provider for `place`, its token service `sts`.
    ///
    /// # Errors
    /// The place cannot be addressed.
    pub fn new(place: Place, sts: Sts) -> Result<Self, String> {
        if !place.root.is_empty() && !is_plain_prefix(&place.root) {
            return Err(format!("{:?} is not a plain prefix ending in '/'", place.root));
        }
        let style = if place.path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
        let bucket =
            Bucket::new(place.endpoint.clone(), style, place.bucket.clone(), place.region.clone())
                .map_err(|e| e.to_string())?;
        let credentials = Credentials::new(place.access_key_id.clone(), place.secret.clone());
        let http = sts.http.clone();
        Ok(Self { place, sts, bucket, credentials, http })
    }

    fn mode(mode: pb::AccessMode) -> Result<Mode, Failure> {
        match mode {
            pb::AccessMode::ACCESS_MODE_WRITE => Ok(Mode::Write),
            pb::AccessMode::ACCESS_MODE_READ => Ok(Mode::Read),
            pb::AccessMode::ACCESS_MODE_UNSPECIFIED => Err(refused("access.mode", "no mode")),
        }
    }

    async fn page(&self, prefix: &str, after: &str, limit: u32) -> Result<pb::ObjectList, Failure> {
        let full = format!("{}{prefix}", self.place.root);
        let start = format!("{}{after}", self.place.root);
        let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
        action.with_prefix(full.as_str());
        if !after.is_empty() {
            action.with_start_after(start.as_str());
        }
        action.with_max_keys(usize::try_from(limit).unwrap_or(1000));
        let r = self.http.get(action.sign(SIGNED_FOR)).send().await.map_err(failed)?;
        let status = r.status();
        let text = r.text().await.map_err(failed)?;
        if !status.is_success() {
            return Err(failed(format!("listing: {status}: {text}")));
        }
        let page = ListObjectsV2::parse_response(&text).map_err(failed)?;
        let mut objects = Vec::new();
        for o in page.contents {
            let Some(key) = o.key.strip_prefix(&self.place.root).filter(|k| k.starts_with(prefix))
            else {
                return Err(failed(format!("the store listed {} outside the prefix", o.key)));
            };
            let modified: Option<jiff::Timestamp> = o.last_modified.parse().ok();
            objects.push(pb::StoredObject {
                key: key.to_owned(),
                size: o.size,
                modified: modified.map(|m| timestamp(SystemTime::from(m))).into(),
                ..Default::default()
            });
        }
        let more = page.next_continuation_token.is_some_and(|t| !t.is_empty());
        Ok(pb::ObjectList { objects, more, ..Default::default() })
    }
}

impl BackupTarget for S3Target {
    async fn grant_access(
        &self,
        prefix: String,
        mode: pb::AccessMode,
        ttl: Duration,
    ) -> Result<pb::ObjectAccess, Failure> {
        if !is_plain_prefix(&prefix) {
            return Err(refused("access.prefix", format!("{prefix:?} is not a plain prefix")));
        }
        if !(SHORTEST..=LONGEST).contains(&ttl) {
            let why = "the token service issues 900 to 43200 seconds, and never longer than asked";
            return Err(refused("access.ttl", why));
        }
        let key_prefix = format!("{}{prefix}", self.place.root);
        let policy = session_policy(&self.place.bucket, &key_prefix, Self::mode(mode)?);
        let t = self.sts.assume(&policy.to_string(), ttl).await.map_err(|e| match e {
            StsError::Refused(m) => refused("store.refused", m),
            StsError::Failed(m) => failed(m),
        })?;
        let s3 = pb::S3Access {
            endpoint: self.place.endpoint.as_str().trim_end_matches('/').to_owned(),
            region: self.place.region.clone(),
            bucket: self.place.bucket.clone(),
            key_prefix,
            access_key_id: t.access_key_id,
            secret_access_key: t.secret_access_key,
            session_token: t.session_token,
            path_style: self.place.path_style,
            ..Default::default()
        };
        Ok(pb::ObjectAccess {
            expires_at: timestamp(t.expires).into(),
            protocol: Some(Protocol::S3(Box::new(s3))),
            ..Default::default()
        })
    }

    async fn list(
        &self,
        prefix: String,
        after: String,
        limit: u32,
    ) -> Result<pb::ObjectList, Failure> {
        if !is_plain_prefix(&prefix) {
            return Err(refused("list.prefix", format!("{prefix:?} is not a plain prefix")));
        }
        self.page(&prefix, &after, limit.clamp(1, 1000)).await
    }

    async fn delete(&self, keys: Vec<String>) -> Result<u32, Failure> {
        if let Some(bad) = keys.iter().find(|k| !is_object_name(k)) {
            return Err(refused("delete.key", format!("{bad:?} is not an object name")));
        }
        if keys.is_empty() {
            return Ok(0);
        }
        let objects: Vec<ObjectIdentifier> =
            keys.iter().map(|k| ObjectIdentifier::new(format!("{}{k}", self.place.root))).collect();
        let action = self.bucket.delete_objects(Some(&self.credentials), objects.iter());
        let url = action.sign(SIGNED_FOR);
        let (body, md5) = action.body_with_md5();
        let r = self.http.post(url).header("content-md5", md5).body(body).send().await;
        let r = r.map_err(failed)?;
        let status = r.status();
        let text = r.text().await.map_err(failed)?;
        if !status.is_success() {
            return Err(failed(format!("deleting: {status}: {text}")));
        }
        let answer = DeleteObjectsResponse::parse(&text).map_err(failed)?;
        if let Some(e) = answer.errors.first() {
            return Err(failed(format!("deleting {}: {} {}", e.key, e.code, e.message)));
        }
        Ok(u32::try_from(answer.deleted.len()).unwrap_or(u32::MAX))
    }
}
