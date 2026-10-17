// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rate limits on the client API (§10 of docs/10-api.md): an interceptor every client service
//! passes through before its handler. It authenticates the request once, charges it to the
//! caller's buckets, hands the caller on to the handler, and says on every response where each
//! bucket stands. The agent's and the relays' services, and the server's own description, are
//! not charged here: they authenticate by their own proofs, or not at all.

use super::{Api, error};
use connectrpc::interceptor::{StreamRequest, StreamResponse, UnaryRequest, UnaryResponse};
use connectrpc::{ConnectError, Interceptor, Next, NextStream, PayloadStream, RequestContext};
use http::{HeaderMap, HeaderValue};
use korpis_app::AppError;
use korpis_core::audit::failure::Attempt;
use korpis_core::authz::ObjectRef;
use korpis_core::rate::Standing;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

/// The procedures, by path prefix, no bucket is charged for.
const UNCHARGED: [&str; 4] = [
    "/korpis.agent.",
    "/korpis.relay.",
    "/korpis.v1.RelayService/",
    "/korpis.v1.ServerInfoService/GetServerInfo",
];

/// The procedures anyone may call without a credential, whose every request is charged to its
/// source instead: each sign-in or redemption the account service begins writes a challenge,
/// a refresh credential is presented without another, and a CLI begins and polls its device
/// authorization before it has one; and the sign-in page's theme, which reads the store.
const ANONYMOUS: [&str; 6] = [
    "/korpis.v1.AccountService/",
    "/korpis.v1.ServerInfoService/GetSignInTheme",
    "/korpis.v1.SessionService/Exchange",
    "/korpis.v1.SessionService/SignOut",
    "/korpis.v1.SessionService/AuthorizeDevice",
    "/korpis.v1.SessionService/DeviceToken",
];

/// The procedures charged to the expensive budget as well: those that compute a plan, take or
/// restore a backup or a snapshot, or install an extension.
const EXPENSIVE: [&str; 12] = [
    "/korpis.v1.WorkloadService/DeclareIntent",
    "/korpis.v1.WorkloadService/Start",
    "/korpis.v1.WorkloadService/Stop",
    "/korpis.v1.WorkloadService/Restart",
    "/korpis.v1.WorkloadService/DeleteWorkload",
    "/korpis.v1.WorkloadService/PlaceWorkload",
    "/korpis.v1.BackupService/CreateBackup",
    "/korpis.v1.BackupService/RestoreBackup",
    "/korpis.v1.VolumeService/CreateSnapshot",
    "/korpis.v1.VolumeService/RollbackVolume",
    "/korpis.v1.ExtensionService/InstallExtension",
    "/korpis.v1.ExtensionService/UpgradeExtension",
];

/// The interceptor that charges each client request to its caller's buckets.
#[derive(Debug)]
pub struct Charging {
    api: Arc<Api>,
}

impl Charging {
    /// Charges the requests `api` serves.
    #[must_use]
    pub fn new(api: Arc<Api>) -> Self {
        Self { api }
    }

    /// Charges the request `ctx` carries, handing its caller on to the handler; where its buckets
    /// stand, or the refusal. A request presenting no credential is not charged to a caller: its
    /// handler refuses it, or serves it if it needs none, and one to the account service is
    /// charged to its source. One presenting a credential that opens nothing is a failed
    /// attempt, charged to its source's failure bucket (§10 of docs/10-api.md).
    async fn admit(&self, ctx: &mut RequestContext) -> Result<Vec<Standing>, ConnectError> {
        let Some(path) = ctx.path().map(str::to_owned) else { return Ok(Vec::new()) };
        if UNCHARGED.iter().any(|p| path.starts_with(p)) {
            return Ok(Vec::new());
        }
        let presented = super::bearer(ctx);
        let ip = ctx.peer_addr().map(|a| a.ip());
        if presented.is_empty() {
            if !ANONYMOUS.iter().any(|p| path.starts_with(p)) {
                return Ok(Vec::new());
            }
            return self.api.app().charge_source(ip).await.map_err(refused);
        }
        let app = self.api.app();
        let caller = match app.authenticate(presented, ip).await {
            Ok(caller) => caller,
            Err(e) => return Err(self.api.failed(e, (Attempt::Credential, ip, None)).await),
        };
        let expensive = EXPENSIVE.contains(&path.as_str());
        let standing = app.charge_request(&caller, expensive).await.map_err(refused)?;
        ctx.extensions_mut().insert(caller);
        Ok(standing)
    }
}

impl Api {
    /// The answer to an attempt to authenticate that failed with `e`: a failure of `attempt` from
    /// `ip`, naming `target`, is charged to its source and recorded, and answered
    /// `UNAUTHENTICATED`, or `RATE_LIMITED` once its source's bucket refuses; anything else is
    /// answered as it is.
    pub(super) async fn failed(
        &self,
        e: AppError,
        (attempt, ip, target): (Attempt, Option<IpAddr>, Option<ObjectRef>),
    ) -> ConnectError {
        match e {
            AppError::Unauthenticated => {
                refused(self.app().authentication_failed(attempt, ip, target).await)
            }
            e @ AppError::RateLimited { .. } => refused(e),
            other => error::from_app(other),
        }
    }
}

/// `e` on the wire, a refusal by a bucket carrying where each bucket stands and `Retry-After`.
pub(super) fn refused(e: AppError) -> ConnectError {
    let AppError::RateLimited { retry_after, standing } = e else { return error::from_app(e) };
    let mut refused = error::rate_limited(retry_after);
    let headers = refused.response_headers_mut();
    headers.extend(standing_headers(&standing));
    if let Ok(v) = HeaderValue::from_str(&seconds(retry_after).to_string()) {
        headers.insert(http::header::RETRY_AFTER, v);
    }
    refused
}

#[connectrpc::async_trait]
impl Interceptor for Charging {
    async fn intercept_unary(
        &self,
        mut req: UnaryRequest,
        next: Next<'_>,
    ) -> Result<UnaryResponse, ConnectError> {
        let standing = self.admit(&mut req.ctx).await?;
        let mut response = next.run(req).await?;
        response.headers.extend(standing_headers(&standing));
        Ok(response)
    }

    async fn intercept_streaming(
        &self,
        mut req: StreamRequest,
        inbound: PayloadStream,
        next: NextStream<'_>,
    ) -> Result<StreamResponse, ConnectError> {
        let standing = self.admit(&mut req.ctx).await?;
        let mut response = next.run(req, inbound).await?;
        response.headers.extend(standing_headers(&standing));
        Ok(response)
    }
}

/// `RateLimit-Policy` and `RateLimit`, as the IETF HTTP API working group's draft names them: for
/// each policy, the bucket of it furthest from full, since a policy may be charged in several
/// organizations at once and the caller is bounded by the emptiest.
fn standing_headers(standing: &[Standing]) -> HeaderMap {
    let mut tightest: Vec<&Standing> = Vec::new();
    for s in standing {
        match tightest.iter_mut().find(|t| t.bucket.policy == s.bucket.policy) {
            Some(t) if s.full_in > t.full_in => *t = s,
            Some(_) => {}
            None => tightest.push(s),
        }
    }
    let mut headers = HeaderMap::new();
    if tightest.is_empty() {
        return headers;
    }
    let policy = tightest.iter().map(|s| {
        let rate = s.bucket.rate;
        format!("\"{}\";q={};w={}", s.bucket.policy, rate.burst, seconds(rate.window()))
    });
    let limit = tightest
        .iter()
        .map(|s| format!("\"{}\";r={};t={}", s.bucket.policy, s.remaining(), seconds(s.full_in)));
    for (name, items) in
        [("ratelimit-policy", policy.collect::<Vec<_>>()), ("ratelimit", limit.collect())]
    {
        if let Ok(v) = HeaderValue::from_str(&items.join(", ")) {
            headers.insert(name, v);
        }
    }
    headers
}

/// Whole seconds, rounded up: a client told zero would retry at once.
fn seconds(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}
