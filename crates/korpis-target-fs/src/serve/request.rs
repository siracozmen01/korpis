// SPDX-License-Identifier: AGPL-3.0-or-later

//! One request: its presigned signature checked against its grant's secret, its session token
//! against the grant's, and what it names against what the grant reaches; then the object got,
//! its size read, the object put, or the bucket listed. Nothing is ever deleted over this path.

use super::{Served, xml};
use crate::target::BUCKET;
use crate::tokens::{Grant, Mode};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use korpis_extension::sigv4::{Presigned, verify_presigned};
use std::time::SystemTime;

/// The largest object put.
const LARGEST: usize = 64 << 20;

/// Why a request is answered with an error.
#[derive(Debug)]
struct Refused {
    status: StatusCode,
    code: &'static str,
    message: String,
}

fn denied(message: impl std::fmt::Display) -> Refused {
    Refused { status: StatusCode::FORBIDDEN, code: "AccessDenied", message: message.to_string() }
}

fn failed(message: impl std::fmt::Display) -> Refused {
    let status = StatusCode::INTERNAL_SERVER_ERROR;
    Refused { status, code: "InternalError", message: message.to_string() }
}

fn response(status: StatusCode, body: Bytes) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(body));
    *r.status_mut() = status;
    r
}

/// The answer to `r`.
pub(super) async fn answer(served: &Served, r: Request<Incoming>) -> Response<Full<Bytes>> {
    handle(served, r).await.unwrap_or_else(|e| {
        let mut r = response(e.status, Bytes::from(xml::error(e.code, &e.message)));
        r.headers_mut()
            .insert("content-type", hyper::header::HeaderValue::from_static("application/xml"));
        r
    })
}

/// Decodes `%XX` escapes.
fn decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut rest = s.as_bytes();
    while let Some((&first, after)) = rest.split_first() {
        rest = after;
        if first != b'%' {
            out.push(first);
            continue;
        }
        let (digits, after) = rest.split_at_checked(2)?;
        out.push(u8::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?);
        rest = after;
    }
    String::from_utf8(out).ok()
}

/// The object `path` names, or `None` for the bucket itself.
fn named(path: &str) -> Result<Option<String>, Refused> {
    let path = decode(path).ok_or_else(|| denied("the path"))?;
    let rest = path.strip_prefix('/').and_then(|p| p.strip_prefix(BUCKET));
    match rest {
        Some("" | "/") => Ok(None),
        Some(r) => Ok(Some(r.strip_prefix('/').ok_or_else(|| denied("the bucket"))?.to_owned())),
        None => Err(Refused {
            status: StatusCode::NOT_FOUND,
            code: "NoSuchBucket",
            message: "one bucket is served".into(),
        }),
    }
}

/// The grant `r` was presigned with, if its signature and token hold now.
fn granted(served: &Served, r: &hyper::http::request::Parts) -> Result<Grant, Refused> {
    let now = SystemTime::now();
    let headers: Vec<(String, String)> = r
        .headers
        .iter()
        .filter_map(|(n, v)| Some((n.as_str().to_owned(), v.to_str().ok()?.to_owned())))
        .collect();
    let presigned = Presigned {
        method: r.method.as_str(),
        path: r.uri.path(),
        query: r.uri.query().unwrap_or_default(),
        headers: &headers,
    };
    let tokens = &served.tokens;
    let v = verify_presigned(&presigned, now, |k| tokens.get(k, now).map(|g| g.secret))
        .map_err(denied)?;
    let grant = tokens.get(&v.access_key_id, now).ok_or_else(|| denied("the grant ended"))?;
    if v.session_token != grant.session_token {
        return Err(denied("the session token is not the grant's"));
    }
    Ok(grant)
}

async fn handle(served: &Served, r: Request<Incoming>) -> Result<Response<Full<Bytes>>, Refused> {
    let (parts, body) = r.into_parts();
    let grant = granted(served, &parts)?;
    let object = named(parts.uri.path())?;
    let beneath = |k: &str| k.len() > grant.prefix.len() && k.starts_with(&grant.prefix);
    let head = parts.method == Method::HEAD;
    match (parts.method, object) {
        (Method::GET | Method::HEAD, Some(k)) if beneath(&k) => get(served, k, head).await,
        (Method::PUT, Some(k)) if beneath(&k) && grant.mode == Mode::Write => {
            put(served, k, body).await
        }
        (Method::GET, None) => list(served, &grant, parts.uri.query().unwrap_or_default()).await,
        (Method::GET | Method::HEAD | Method::PUT, _) => Err(denied("beyond the grant")),
        _ => Err(Refused {
            status: StatusCode::METHOD_NOT_ALLOWED,
            code: "MethodNotAllowed",
            message: "get, put, and list".into(),
        }),
    }
}

async fn get(served: &Served, key: String, head: bool) -> Result<Response<Full<Bytes>>, Refused> {
    let store = served.store.clone();
    let found = tokio::task::spawn_blocking(move || store.get(&key))
        .await
        .map_err(failed)?
        .map_err(failed)?;
    let Some(bytes) = found else {
        let message = "no such object".into();
        return Err(Refused { status: StatusCode::NOT_FOUND, code: "NoSuchKey", message });
    };
    let length = bytes.len();
    let mut r = response(StatusCode::OK, if head { Bytes::new() } else { Bytes::from(bytes) });
    r.headers_mut().insert("content-length", hyper::header::HeaderValue::from(length));
    Ok(r)
}

async fn put(
    served: &Served,
    key: String,
    body: Incoming,
) -> Result<Response<Full<Bytes>>, Refused> {
    let bytes = Limited::new(body, LARGEST).collect().await.map_err(|e| Refused {
        status: StatusCode::BAD_REQUEST,
        code: "EntityTooLarge",
        message: e.to_string(),
    })?;
    let bytes = bytes.to_bytes();
    let store = served.store.clone();
    tokio::task::spawn_blocking(move || store.put(&key, &bytes))
        .await
        .map_err(failed)?
        .map_err(failed)?;
    Ok(response(StatusCode::OK, Bytes::new()))
}

async fn list(
    served: &Served,
    grant: &Grant,
    query: &str,
) -> Result<Response<Full<Bytes>>, Refused> {
    let mut asked: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for part in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        asked.insert(decode(k).unwrap_or_default(), decode(v).unwrap_or_default());
    }
    let prefix = asked.get("prefix").cloned().unwrap_or_default();
    if !prefix.starts_with(&grant.prefix) {
        return Err(denied("a listing beyond the grant"));
    }
    let token = asked.get("continuation-token").map(|t| {
        let pairs = t.as_bytes().chunks(2);
        let bytes: Option<Vec<u8>> =
            pairs.map(|d| u8::from_str_radix(std::str::from_utf8(d).ok()?, 16).ok()).collect();
        bytes.and_then(|b| String::from_utf8(b).ok()).unwrap_or_default()
    });
    let after = token.or_else(|| asked.get("start-after").cloned()).unwrap_or_default();
    let max_keys = asked.get("max-keys").and_then(|m| m.parse().ok()).unwrap_or(1000_usize);
    let max_keys = max_keys.clamp(1, 1000);
    let store = served.store.clone();
    let listed = prefix.clone();
    let page = tokio::task::spawn_blocking(move || store.list(&listed, &after, max_keys))
        .await
        .map_err(failed)?
        .map_err(failed)?;
    let url_encoded = asked.get("encoding-type").is_some_and(|e| e == "url");
    let shown = xml::Asked { bucket: BUCKET, prefix: &prefix, max_keys, url_encoded };
    let mut r = response(StatusCode::OK, Bytes::from(xml::listing(&shown, &page)));
    r.headers_mut()
        .insert("content-type", hyper::header::HeaderValue::from_static("application/xml"));
    Ok(r)
}
