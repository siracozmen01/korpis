// SPDX-License-Identifier: AGPL-3.0-or-later

//! The client API (docs/10-api.md): a driving adapter. Each handler authenticates the caller,
//! translates the request into domain values, calls one use case, and translates the answer
//! back. Handlers hold no rule; an `if` here that is not about the wire is in the wrong crate.

// Handlers are `async fn`, which the lint reads as refining the generated traits' `impl Future`
// signatures. The futures are Send (the router requires it), and nothing outside relies on the
// refinement.
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod agent;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod audit;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod authz;
pub mod error;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod info;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod node;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod plan;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod set;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod tenancy;
mod trust;
pub mod wire;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod workload;

use connectrpc::{ConnectError, RequestContext, Response, Router, ServiceResult};
use korpis_app::ports::SystemClock;
use korpis_app::{App, Caller, Page, Reply};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::{
    DeclaredSetServiceExt, EffectServiceExt, GrantServiceExt, NodeServiceExt,
    OrganizationServiceExt, PlanServiceExt, ProjectServiceExt, ServerInfoServiceExt,
    WorkloadServiceExt,
};
use korpis_recipe::source::RegistrySource;
use korpis_store::PgStore;
use std::sync::Arc;
use uuid::Uuid;
use wire::Refuse;

/// The application as the server composes it: PostgreSQL, the system clock, OCI registries.
pub type ServerApp = App<PgStore, SystemClock, RegistrySource>;

/// The services' shared state: the application.
#[derive(Debug)]
pub struct Api {
    app: Arc<ServerApp>,
    /// What enrollment tokens tell agents to pin.
    ca_fingerprint: String,
    /// The agent sessions this process holds.
    sessions: Arc<crate::sessions::Sessions>,
}

impl Api {
    /// The API over `app`.
    #[must_use]
    pub fn new(app: ServerApp) -> Self {
        Self {
            app: Arc::new(app),
            ca_fingerprint: String::new(),
            sessions: Arc::new(crate::sessions::Sessions::default()),
        }
    }

    /// Tells enrolling agents to pin the CA with `fingerprint` (§3 of docs/18-operations.md).
    #[must_use]
    pub fn with_ca_fingerprint(mut self, fingerprint: String) -> Self {
        self.ca_fingerprint = fingerprint;
        self
    }

    /// The agent sessions, for the composition root's background work.
    #[must_use]
    pub fn sessions(&self) -> &crate::sessions::Sessions {
        &self.sessions
    }

    /// The application, for the composition root's background work.
    #[must_use]
    pub fn app(&self) -> &ServerApp {
        &self.app
    }

    /// Registers every client service, and the agent service, on a router.
    #[must_use]
    pub fn router(self: Arc<Self>) -> Router {
        let router = Router::new();
        let router = ServerInfoServiceExt::register(Arc::clone(&self), router);
        let router = OrganizationServiceExt::register(Arc::clone(&self), router);
        let router = ProjectServiceExt::register(Arc::clone(&self), router);
        let router = GrantServiceExt::register(Arc::clone(&self), router);
        let router = WorkloadServiceExt::register(Arc::clone(&self), router);
        let router = PlanServiceExt::register(Arc::clone(&self), router);
        let router = NodeServiceExt::register(Arc::clone(&self), router);
        let router = EffectServiceExt::register(Arc::clone(&self), router);
        let router = DeclaredSetServiceExt::register(Arc::clone(&self), router);
        korpis_proto::korpis::agent::v1::AgentServiceExt::register(self, router)
    }

    /// The caller of a request: the bearer API token, from the peer address.
    async fn caller(&self, ctx: &RequestContext) -> Result<Caller, ConnectError> {
        let presented = ctx
            .headers()
            .get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or_default();
        self.app
            .authenticate(presented, ctx.peer_addr().map(|a| a.ip()))
            .await
            .map_err(error::from_app)
    }
}

/// The idempotency key every mutation carries (§6 of docs/10-api.md).
fn mutation_key(m: Option<&pb::Mutation>) -> Result<&str, ConnectError> {
    let m = m.ok_or_else(|| error::invalid("mutation", "required"))?;
    if m.idempotency_key.is_empty() {
        return Err(error::invalid("mutation.idempotency_key", "required"));
    }
    Ok(&m.idempotency_key)
}

/// Encodes a response for the idempotency record.
fn encode<R: buffa::Message>(r: &R) -> Vec<u8> {
    r.encode_to_vec()
}

/// The response to a mutation: built from a fresh result, or decoded from the recorded bytes of
/// the original.
fn respond<T, R: buffa::Message + Send + 'static>(
    reply: Reply<T>,
    build: impl FnOnce(&T) -> R,
) -> ServiceResult<R> {
    match reply {
        Reply::Fresh(t) => Response::ok(build(&t)),
        Reply::Replayed(bytes) => {
            Response::ok(R::decode_from_slice(&bytes).map_err(|e| error::internal(&e))?)
        }
    }
}

/// A page request.
fn page(p: Option<&pb::PageRequest>, kind: IdKind) -> Result<Page, ConnectError> {
    let (size, token) = p.map_or((0, ""), |p| (p.page_size, p.page_token.as_str()));
    let after = if token.is_empty() {
        None
    } else {
        Some(wire::id(kind, "page.page_token", token).refuse()?)
    };
    Ok(Page::new(after, size))
}

/// The page response for a next cursor.
fn next_page(next: Option<Uuid>, kind: IdKind) -> pb::PageResponse {
    pb::PageResponse {
        next_page_token: next.map(|n| wire::wire_id(kind, n)).unwrap_or_default(),
        ..Default::default()
    }
}
