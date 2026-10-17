// SPDX-License-Identifier: AGPL-3.0-or-later

//! What answers on the control plane's address (§1 of docs/11-surface-web.md): a procedure of
//! the API goes to the API, with the cross-origin headers a panel served from a listed origin
//! needs; `/about/source` says where this build's source is (§7 of docs/11-surface-web.md); and
//! every other path is a file of the panel in `--web-root`, if one is served.

use crate::{about, web};
use bytes::Bytes;
use connectrpc::{ConnectRpcBody, ConnectRpcService};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::{Either, Full};
use std::convert::Infallible;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower_service::Service;

/// What the front answers with: the API's body, or one it made itself.
pub type Body = Either<ConnectRpcBody, Full<Bytes>>;

/// The headers a cross-origin call may send, besides the safelisted ones: Connect's and gRPC-Web's.
const ALLOW_HEADERS: &str = "authorization, content-type, connect-protocol-version, \
    connect-timeout-ms, connect-content-encoding, connect-accept-encoding, grpc-timeout, \
    x-grpc-web, x-user-agent, idempotency-key";
/// The headers a cross-origin caller may read: the API's errors, limits, and trailers.
const EXPOSE_HEADERS: &str = "grpc-status, grpc-message, grpc-status-details-bin, \
    ratelimit-limit, ratelimit-remaining, ratelimit-reset, retry-after";

/// The control plane's address.
#[derive(Clone)]
pub struct Front {
    api: ConnectRpcService,
    web_root: Option<Arc<PathBuf>>,
    origins: Arc<[String]>,
}

impl std::fmt::Debug for Front {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Front")
            .field("web_root", &self.web_root)
            .field("origins", &self.origins)
            .finish_non_exhaustive()
    }
}

impl Front {
    /// `api` on the address, the panel's files from `web_root`, and cross-origin calls answered
    /// for `origins` alone.
    #[must_use]
    pub fn new(api: ConnectRpcService, web_root: Option<PathBuf>, origins: Vec<String>) -> Self {
        Self { api, web_root: web_root.map(Arc::new), origins: origins.into() }
    }

    /// The origin `headers` name, if it is one cross-origin calls are answered for.
    fn listed(&self, headers: &HeaderMap) -> Option<HeaderValue> {
        let origin = headers.get(header::ORIGIN)?;
        let text = origin.to_str().ok()?;
        self.origins.iter().any(|o| o == text).then(|| origin.clone())
    }
}

/// Whether `path` is a procedure: `/package.Service/Method`.
fn procedure(path: &str) -> bool {
    let mut parts = path.trim_start_matches('/').split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(s), Some(m), None) if s.contains('.') && !m.is_empty())
}

fn full(r: Response<Bytes>) -> Response<Body> {
    r.map(|b| Either::Right(Full::new(b)))
}

/// The headers that let `origin` call with credentials and read the answer.
fn allow(headers: &mut HeaderMap, origin: HeaderValue) {
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(header::ACCESS_CONTROL_ALLOW_CREDENTIALS, HeaderValue::from_static("true"));
    headers.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static(EXPOSE_HEADERS));
    headers.append(header::VARY, HeaderValue::from_static("origin"));
}

/// The answer to a preflight: allowed for a listed origin, refused for any other.
fn preflight(origin: Option<HeaderValue>) -> Response<Bytes> {
    let mut r = Response::new(Bytes::new());
    let Some(origin) = origin else {
        *r.status_mut() = StatusCode::FORBIDDEN;
        return r;
    };
    *r.status_mut() = StatusCode::NO_CONTENT;
    let h = r.headers_mut();
    allow(h, origin);
    h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("POST, GET"));
    h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static(ALLOW_HEADERS));
    h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    r
}

impl<B> Service<Request<B>> for Front
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        let path = request.uri().path().to_owned();
        let origin = self.listed(request.headers());
        if !procedure(&path) {
            let (method, root) = (request.method().clone(), self.web_root.clone());
            return Box::pin(files(root, method, path));
        }
        if request.method() == Method::OPTIONS {
            return Box::pin(std::future::ready(Ok(full(preflight(origin)))));
        }
        Box::pin(api(self.api.call(request), origin))
    }
}

/// The API's answer, with the headers a listed `origin` needs to read it.
async fn api(
    answer: impl Future<Output = Result<Response<ConnectRpcBody>, Infallible>>,
    origin: Option<HeaderValue>,
) -> Result<Response<Body>, Infallible> {
    let Ok(mut r) = answer.await;
    if let Some(origin) = origin {
        allow(r.headers_mut(), origin);
    }
    Ok(r.map(Either::Left))
}

/// What a path that is not the API's names: the source page, or a file of the panel in `root`.
async fn files(
    root: Option<Arc<PathBuf>>,
    method: Method,
    path: String,
) -> Result<Response<Body>, Infallible> {
    if path == about::PATH {
        return Ok(full(about::page(root.as_deref().map(PathBuf::as_path)).await));
    }
    let Some(root) = root else { return Ok(full(web::response(web::alone()))) };
    Ok(full(web::response(web::serve(&root, &method, &path).await)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_procedure_is_a_service_and_a_method_and_nothing_else() {
        assert!(procedure("/korpis.v1.WorkloadService/GetWorkload"));
        assert!(!procedure("/favicon.ico"));
        assert!(!procedure("/assets/index-1.js"));
        assert!(!procedure("/korpis.v1.WorkloadService/GetWorkload/more"));
        assert!(!procedure("/"));
    }
}
