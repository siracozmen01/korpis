// SPDX-License-Identifier: AGPL-3.0-or-later

//! The client API (docs/10-api.md): a driving adapter. Each handler authenticates the caller,
//! translates the request into domain values, calls one use case, and translates the answer
//! back. Handlers hold no rule; an `if` here that is not about the wire is in the wrong crate.

// Handlers are `async fn`, which the lint reads as refining the generated traits' `impl Future`
// signatures. The futures are Send (the router requires it), and nothing outside relies on the
// refinement.
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod account;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod agent;
mod assert;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod audit;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod authz;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod backup;
mod backup_read;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod binding;
mod ceiling;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod certificate;
mod cookie;
pub mod cursor;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod data;
mod device;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod domain;
pub mod error;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod event;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod extension;
mod external;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod info;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod metering;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod node;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod plan;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod profile;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod provider;
mod quota;
mod rate;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod relay;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod relay_control;
mod relay_watch;
mod relays;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod repository;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod run;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod session;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod set;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod stream;
mod stream_offloaded;
mod suspension;
mod template;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod tenancy;
mod theme;
mod trust;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod volume;
pub mod wire;
#[expect(refining_impl_trait, reason = "async fn handlers for generated traits")]
mod workload;

use crate::relay::in_process::InProcess;
use connectrpc::{ConnectError, RequestContext, Response, Router, ServiceResult};
use korpis_app::ports::SystemClock;
use korpis_app::{App, Caller, Page, Reply};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::{
    AccountServiceExt, BackupServiceExt, CertificateServiceExt, DeclaredSetServiceExt,
    DomainServiceExt, EffectServiceExt, EventServiceExt, ExtensionServiceExt, GrantServiceExt,
    IdentityServiceExt, MeteringServiceExt, NodeServiceExt, OrganizationServiceExt, PlanServiceExt,
    ProfileServiceExt, ProjectServiceExt, ProviderServiceExt, RepositoryServiceExt, RunServiceExt,
    ServerInfoServiceExt, SessionServiceExt, StreamServiceExt, VolumeServiceExt,
    WorkloadServiceExt,
};
use korpis_recipe::source::RegistrySource;
use korpis_store::PgStore;
pub use rate::Charging;
pub use relay::RelayRole;
use std::sync::Arc;

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
    /// The provider streams this process holds.
    providers: Arc<crate::providers::Providers>,
    /// The revocation list this process publishes to its sessions.
    revocations: Arc<crate::revocations::Revocations>,
    /// The relay role in this process: attached data connections and the sessions they carry.
    relay: Arc<RelayRole<InProcess>>,
    /// Whether this process serves the relay role (§3.1 of docs/02-architecture.md).
    serves_relay: bool,
    /// The relays this process names to the nodes it holds sessions with.
    named_relays: Arc<crate::named_relays::NamedRelays>,
    /// The key page tokens are sealed under (§8 of docs/10-api.md).
    cursors: Arc<cursor::Cursors>,
    /// Until when a restore settles, as last asked of the store, and when that was.
    settling: std::sync::Mutex<Option<(std::time::Instant, Option<std::time::SystemTime>)>>,
}

impl Api {
    /// The API over `app`.
    #[must_use]
    pub fn new(app: ServerApp) -> Self {
        let sessions = Arc::new(crate::sessions::Sessions::default());
        let asking = crate::feasibility::SessionFeasibility::new(Arc::clone(&sessions));
        let app = Arc::new(app.with_feasibility(Arc::new(asking)));
        let revocations = Arc::new(crate::revocations::Revocations::default());
        let in_process = InProcess::new(Arc::clone(&app), Arc::clone(&revocations));
        Self {
            app,
            ca_fingerprint: String::new(),
            sessions,
            providers: Arc::new(crate::providers::Providers::default()),
            revocations,
            relay: Arc::new(RelayRole::new(Arc::new(in_process))),
            serves_relay: true,
            named_relays: Arc::default(),
            cursors: Arc::new(cursor::Cursors::of_this_process()),
            settling: std::sync::Mutex::new(None),
        }
    }

    /// Seals page tokens under the installation's key, so every control plane opens what
    /// another sealed (§8 of docs/10-api.md).
    #[must_use]
    pub fn with_cursor_key(mut self, key: [u8; 32]) -> Self {
        self.cursors = Arc::new(cursor::Cursors::new(key));
        self
    }

    /// Whether this process serves the relay role; it does unless told not to.
    #[must_use]
    pub fn with_relay_role(mut self, serves: bool) -> Self {
        self.serves_relay = serves;
        self
    }

    /// Whether this process serves the relay role.
    #[must_use]
    pub fn serves_relay(&self) -> bool {
        self.serves_relay
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

    /// The provider streams, for the composition root's background work.
    #[must_use]
    pub fn providers(&self) -> &crate::providers::Providers {
        &self.providers
    }

    /// The revocation list published to the sessions, for the composition root's background
    /// work.
    #[must_use]
    pub fn revocations(&self) -> &crate::revocations::Revocations {
        &self.revocations
    }

    /// The relays named to the nodes, for the composition root's background work.
    #[must_use]
    pub fn named_relays(&self) -> &crate::named_relays::NamedRelays {
        &self.named_relays
    }

    /// The relay role in this process, for the composition root to give it the token keys.
    #[must_use]
    pub fn relay(&self) -> &InProcess {
        self.relay.verifier()
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
        let router = IdentityServiceExt::register(Arc::clone(&self), router);
        let router = WorkloadServiceExt::register(Arc::clone(&self), router);
        let router = PlanServiceExt::register(Arc::clone(&self), router);
        let router = NodeServiceExt::register(Arc::clone(&self), router);
        let router = EffectServiceExt::register(Arc::clone(&self), router);
        let router = MeteringServiceExt::register(Arc::clone(&self), router);
        let router = DeclaredSetServiceExt::register(Arc::clone(&self), router);
        let router = ExtensionServiceExt::register(Arc::clone(&self), router);
        let router = EventServiceExt::register(Arc::clone(&self), router);
        let router = ProviderServiceExt::register(Arc::clone(&self), router);
        let router = StreamServiceExt::register(Arc::clone(&self), router);
        let router = VolumeServiceExt::register(Arc::clone(&self), router);
        let router = RepositoryServiceExt::register(Arc::clone(&self), router);
        let router = DomainServiceExt::register(Arc::clone(&self), router);
        let router = CertificateServiceExt::register(Arc::clone(&self), router);
        let router = BackupServiceExt::register(Arc::clone(&self), router);
        let router = RunServiceExt::register(Arc::clone(&self), router);
        let router = AccountServiceExt::register(Arc::clone(&self), router);
        let router = SessionServiceExt::register(Arc::clone(&self), router);
        let router = ProfileServiceExt::register(Arc::clone(&self), router);
        let router =
            if self.serves_relay { Arc::clone(&self.relay).register(router) } else { router };
        let router = korpis_proto::korpis::relay::v1::RelayControlServiceExt::register(
            Arc::clone(&self),
            router,
        );
        korpis_proto::korpis::agent::v1::AgentServiceExt::register(self, router)
    }

    /// The caller of a request: as the rate limit interceptor authenticated it, or else the bearer
    /// API token, from the peer address.
    async fn caller(&self, ctx: &RequestContext) -> Result<Caller, ConnectError> {
        self.not_settling().await?;
        let caller = match ctx.extensions().get::<Caller>() {
            Some(caller) => caller.clone(),
            None => self.authenticated(ctx).await?,
        };
        // A client that relayed what a person presented acts for them with the ticket it was
        // handed, beside its own credential (§5.2 of docs/08-identity.md).
        match ctx.headers().get(RELAYED_AS) {
            Some(ticket) => {
                let ticket = ticket.to_str().unwrap_or_default();
                self.app.relayed(&caller, ticket).await.map_err(error::from_app)
            }
            None => Ok(caller),
        }
    }

    /// Refused while a restore settles (§8 of docs/03-state.md): until what the agents present
    /// is recovered, a caller would act on the backup's state, and a declaration could take a
    /// version a node is running. Agents and relays are served, and so is what needs no caller.
    /// Asked of the store at most once a second.
    async fn not_settling(&self) -> Result<(), ConnectError> {
        let fresh = |at: &std::time::Instant| at.elapsed() < std::time::Duration::from_secs(1);
        let known = *self.settling.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let until = match known {
            Some((at, until)) if fresh(&at) => until,
            _ => {
                let until = self.app.settling().await.map_err(error::from_app)?;
                let mut held =
                    self.settling.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                *held = Some((std::time::Instant::now(), until));
                until
            }
        };
        match until {
            Some(until) if until > self.app.now() => {
                Err(error::from_app(korpis_app::AppError::Unavailable {
                    what: "the control plane, settling after a restore,",
                    detail: format!("settling until {until:?}"),
                }))
            }
            _ => Ok(()),
        }
    }

    /// The bearer API token's caller, from the peer address.
    async fn authenticated(&self, ctx: &RequestContext) -> Result<Caller, ConnectError> {
        self.app
            .authenticate(bearer(ctx), ctx.peer_addr().map(|a| a.ip()))
            .await
            .map_err(error::from_app)
    }
}

/// The header a relaying client sends the ticket it was handed in.
pub const RELAYED_AS: &str = "korpis-as";

/// The bearer credential a request presents; empty if it presents none.
fn bearer(ctx: &RequestContext) -> &str {
    ctx.headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
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
