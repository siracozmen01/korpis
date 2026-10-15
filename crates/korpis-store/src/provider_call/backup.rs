// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backup target calls in the stored queue (§5.6 of docs/06-storage.md). Access carries a
//! secret, which the record keeps only until its caller has read it (§5.6): a redacted access
//! reads back with its secret and session token empty.

use korpis_core::extension::{
    AccessMode, CallOutcome, CallRequest, ObjectAccess, S3Access, StoredObject,
};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn at(v: &Value, key: &str) -> Option<SystemTime> {
    Some(UNIX_EPOCH + Duration::from_secs(v.get(key)?.as_u64()?))
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

const fn mode_name(m: AccessMode) -> &'static str {
    match m {
        AccessMode::Write => "write",
        AccessMode::Read => "read",
    }
}

/// A backup target request to store, if `r` is one.
pub(crate) fn request_to(r: &CallRequest) -> Option<Value> {
    Some(match r {
        CallRequest::GrantAccess { prefix, mode, ttl } => json!({ "grant_access": {
            "prefix": prefix, "mode": mode_name(*mode), "ttl": ttl.as_secs() } }),
        CallRequest::ListObjects { prefix, after, limit } => json!({ "list_objects": {
            "prefix": prefix, "after": after, "limit": limit } }),
        CallRequest::DeleteObjects { keys } => json!({ "delete_objects": { "keys": keys } }),
        _ => return None,
    })
}

/// A stored backup target request, if `v` is one.
pub(crate) fn request_from(v: &Value) -> Option<CallRequest> {
    if let Some(g) = v.get("grant_access") {
        let mode = match g.get("mode")?.as_str()? {
            "write" => AccessMode::Write,
            "read" => AccessMode::Read,
            _ => return None,
        };
        let ttl = Duration::from_secs(g.get("ttl")?.as_u64()?);
        return Some(CallRequest::GrantAccess { prefix: text(g, "prefix")?, mode, ttl });
    }
    if let Some(l) = v.get("list_objects") {
        let limit = u32::try_from(l.get("limit")?.as_u64()?).ok()?;
        return Some(CallRequest::ListObjects {
            prefix: text(l, "prefix")?,
            after: text(l, "after")?,
            limit,
        });
    }
    let keys = v.get("delete_objects")?.get("keys")?.as_array()?;
    let keys = keys.iter().map(|k| k.as_str().map(str::to_owned)).collect::<Option<_>>()?;
    Some(CallRequest::DeleteObjects { keys })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let digits = s.as_bytes().chunks(2);
    digits.map(|d| u8::from_str_radix(std::str::from_utf8(d).ok()?, 16).ok()).collect()
}

/// A backup target outcome to store, if `o` is one.
pub(crate) fn outcome_to(o: &CallOutcome) -> Option<Value> {
    Some(match o {
        CallOutcome::Access(a) => {
            let s = &a.s3;
            json!({ "access": { "expires_at": secs(a.expires_at), "s3": {
                "endpoint": s.endpoint, "region": s.region, "bucket": s.bucket,
                "key_prefix": s.key_prefix, "access_key_id": s.access_key_id,
                "secret_access_key": s.secret_access_key, "session_token": s.session_token,
                "path_style": s.path_style, "served_by": s.served_by,
                "certificate": hex(&s.certificate),
            } } })
        }
        CallOutcome::Objects { objects, more } => {
            let list: Vec<Value> = objects
                .iter()
                .map(|o| json!({ "key": o.key, "size": o.size, "modified": secs(o.modified) }))
                .collect();
            json!({ "objects": { "objects": list, "more": more } })
        }
        CallOutcome::Deleted { count } => json!({ "deleted": { "count": count } }),
        _ => return None,
    })
}

/// A stored backup target outcome, if `v` is one.
pub(crate) fn outcome_from(v: &Value) -> Option<CallOutcome> {
    if let Some(a) = v.get("access") {
        let s = a.get("s3")?;
        // A redacted record has no secret and no token: they read back empty.
        let secret = |k: &str| text(s, k).unwrap_or_default();
        let s3 = S3Access {
            endpoint: text(s, "endpoint")?,
            region: text(s, "region")?,
            bucket: text(s, "bucket")?,
            key_prefix: text(s, "key_prefix")?,
            access_key_id: text(s, "access_key_id")?,
            secret_access_key: secret("secret_access_key"),
            session_token: secret("session_token"),
            path_style: s.get("path_style").and_then(Value::as_bool).unwrap_or(false),
            served_by: text(s, "served_by").unwrap_or_default(),
            certificate: text(s, "certificate").and_then(|h| unhex(&h)).unwrap_or_default(),
        };
        return Some(CallOutcome::Access(ObjectAccess { expires_at: at(a, "expires_at")?, s3 }));
    }
    if let Some(l) = v.get("objects") {
        let objects = l.get("objects")?.as_array()?;
        let objects = objects
            .iter()
            .map(|o| {
                Some(StoredObject {
                    key: text(o, "key")?,
                    size: o.get("size")?.as_u64()?,
                    modified: at(o, "modified")?,
                })
            })
            .collect::<Option<_>>()?;
        return Some(CallOutcome::Objects { objects, more: l.get("more")?.as_bool()? });
    }
    let count = u32::try_from(v.get("deleted")?.get("count")?.as_u64()?).ok()?;
    Some(CallOutcome::Deleted { count })
}
