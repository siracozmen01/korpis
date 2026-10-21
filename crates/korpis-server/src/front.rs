// SPDX-License-Identifier: AGPL-3.0-or-later

//! What answers on the control plane's address (§1 of docs/11-surface-web.md): a procedure of
//! the API goes to the API, with the cross-origin headers a panel served from a listed origin
//! needs; `/about/source` says where this build's source is (§7 of docs/11-surface-web.md); and
//! every other path is a file of the panel in `--web-root`, if one is served.
//!
//! At a portal (§3.6 of docs/07-networking.md) the same is answered, for the portal: a request
//! whose host is not the portal's is refused 421, since the connection proved the portal's name
//! and no other, and a cross-origin call is answered for the portal's own origin alone. A
//! relay's session (`korpis.v1.RelayService`) is also answered for every portal's origin, since
//! a portal's panel opens its consoles at the relays the installation names.

use crate::portals::PortalAt;
use crate::{about, web};
use bytes::Bytes;
use connectrpc::{ConnectRpcBody, ConnectRpcService};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::{Either, Full};
use korpis_proto::korpis::agent::v1 as agent;
use std::convert::Infallible;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::watch;
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

/// The relays this control plane names, as it publishes them.
pub type Relays = watch::Receiver<Option<Arc<agent::Relays>>>;

/// The control plane's address.
#[derive(Clone)]
pub struct Front {
    api: ConnectRpcService,
    web_root: Option<Arc<PathBuf>>,
    origins: Arc<[String]>,
    relays: Option<Relays>,
    portal_origins: Option<watch::Receiver<Arc<[String]>>>,
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
        Self {
            api,
            web_root: web_root.map(Arc::new),
            origins: origins.into(),
            relays: None,
            portal_origins: None,
        }
    }

    /// The same, answering a relay's session for each of `origins` too, the portals' as they
    /// change (§3.6 of docs/07-networking.md).
    #[must_use]
    pub fn with_portal_origins(mut self, origins: watch::Receiver<Arc<[String]>>) -> Self {
        self.portal_origins = Some(origins);
        self
    }

    /// The same, writing the relays `relays` names into the panel's policy as it serves the
    /// page, so the panel may open sessions at them (§3.1 of docs/02-architecture.md).
    #[must_use]
    pub fn with_relays(mut self, relays: Relays) -> Self {
        self.relays = Some(relays);
        self
    }

    /// The origins of the relays named now.
    fn relay_origins(&self) -> Vec<String> {
        let Some(relays) = &self.relays else { return Vec::new() };
        let named = relays.borrow().as_ref().map(|r| r.urls.clone()).unwrap_or_default();
        named.iter().filter_map(|u| origin(u)).collect()
    }

    /// The origin `headers` name, if it is one cross-origin calls to `path` are answered for:
    /// at a portal, its own alone; elsewhere, the listed ones, and for a relay's session every
    /// portal's.
    fn listed(
        &self,
        headers: &HeaderMap,
        path: &str,
        at: Option<&PortalAt>,
    ) -> Option<HeaderValue> {
        let origin = headers.get(header::ORIGIN)?;
        let text = origin.to_str().ok()?;
        if let Some(at) = at {
            return (at.origin == text).then(|| origin.clone());
        }
        let portal = || {
            path.starts_with(RELAY_SESSIONS)
                && self
                    .portal_origins
                    .as_ref()
                    .is_some_and(|o| o.borrow().iter().any(|p| p == text))
        };
        (self.origins.iter().any(|o| o == text) || portal()).then(|| origin.clone())
    }
}

/// The prefix of a relay's session procedures.
const RELAY_SESSIONS: &str = "/korpis.v1.RelayService/";

/// Whether `request` names `name` as its host, by its URI's authority (HTTP/2) or its `Host`
/// header (HTTP/1.1), its port aside.
fn names_host<B>(request: &Request<B>, name: &str) -> bool {
    let host = request.uri().host().map(str::to_owned).or_else(|| {
        let value = request.headers().get(header::HOST)?.to_str().ok()?;
        let authority: http::uri::Authority = value.parse().ok()?;
        Some(authority.host().to_owned())
    });
    host.is_some_and(|h| h.eq_ignore_ascii_case(name))
}

/// A request on a portal's connection naming another host (RFC 9110 §15.5.20).
fn misdirected() -> Response<Bytes> {
    let mut r = Response::new(Bytes::from_static(b"this connection is for another name\n"));
    *r.status_mut() = StatusCode::MISDIRECTED_REQUEST;
    r
}

/// The origin of the URL `url`, `https://host:port` as a policy names it; none if it is not one.
fn origin(url: &str) -> Option<String> {
    let uri: http::Uri = url.parse().ok()?;
    Some(format!("{}://{}", uri.scheme_str()?, uri.authority()?))
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
        let at = request.extensions().get::<PortalAt>().cloned();
        if at.as_ref().is_some_and(|at| !names_host(&request, &at.name)) {
            return Box::pin(std::future::ready(Ok(full(misdirected()))));
        }
        let origin = self.listed(request.headers(), &path, at.as_ref());
        if !procedure(&path) {
            let (method, root) = (request.method().clone(), self.web_root.clone());
            return Box::pin(files(root, method, path, self.relay_origins()));
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
    connect: Vec<String>,
) -> Result<Response<Body>, Infallible> {
    if path == about::PATH {
        return Ok(full(about::page(root.as_deref().map(PathBuf::as_path)).await));
    }
    let Some(root) = root else { return Ok(full(web::response(web::alone()))) };
    Ok(full(web::response(web::serve(&root, &method, &path, &connect).await)))
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

    #[test]
    fn a_request_names_its_host_by_its_authority_or_its_host_header() {
        let by_uri = Request::get("https://panel.reseller.example/x").body(()).unwrap();
        assert!(names_host(&by_uri, "panel.reseller.example"));
        let by_header = Request::get("/x").header("host", "Panel.Reseller.Example:7443").body(());
        assert!(names_host(&by_header.unwrap(), "panel.reseller.example"));
        let other = Request::get("/x").header("host", "korpis.example.com").body(()).unwrap();
        assert!(!names_host(&other, "panel.reseller.example"));
        assert!(!names_host(&Request::get("/x").body(()).unwrap(), "panel.reseller.example"));
    }

    #[test]
    fn a_relay_is_named_in_a_policy_by_its_origin_alone() {
        assert_eq!(
            origin("https://r1.example:7444/x?y").as_deref(),
            Some("https://r1.example:7444")
        );
        assert_eq!(origin("not a url"), None);
    }
}
