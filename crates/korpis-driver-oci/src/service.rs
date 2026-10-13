// SPDX-License-Identifier: AGPL-3.0-or-later

//! The driver protocol (`korpis.driver.v1.DriverService`) answered over containerd: each verb
//! of §4.2 of docs/04-runtimes.md mapped onto [`Containerd`], every spec validated again at
//! `Create` whatever `Prepare` said, and every failure given the code the agent branches on.

use crate::containerd::{Containerd, signal_number};
use crate::describe::info;
use crate::error::DriverError;
use crate::prepare::prepare;
use crate::validate::{Policy, validate};
use connectrpc::{ConnectError, ErrorCode, RequestContext, Response, ServiceRequest};
use connectrpc::{ServiceResult, ServiceStream};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::future::{Future, ready};
use std::time::Duration;
use tokio::sync::broadcast;

/// The grace a stop gets when the request names none.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(10);
/// How many events a slow listener may fall behind by before it misses some.
const EVENT_BACKLOG: usize = 1024;

/// The driver's service.
#[derive(Debug)]
pub struct Driver {
    containerd: Containerd,
    policy: Policy,
    events: broadcast::Sender<driver::EventsResponse>,
}

/// A driver failure as the protocol's error. A refusal is a security event (§6.1 of
/// docs/02-architecture.md), so it is logged where it happens as well as answered.
fn status(e: DriverError) -> ConnectError {
    let (code, message) = match e {
        DriverError::Refused(v) => {
            tracing::warn!(violation = %v, "a spec was refused");
            (ErrorCode::PermissionDenied, format!("refused: {v}"))
        }
        DriverError::Rejected(m) => (ErrorCode::FailedPrecondition, m),
        DriverError::NotFound(id) => (ErrorCode::NotFound, format!("no runtime object {id}")),
        DriverError::Unavailable(m) => (ErrorCode::Unavailable, format!("containerd: {m}")),
        DriverError::Failed(m) => (ErrorCode::Internal, m),
    };
    ConnectError::new(code, message)
}

/// The next event for a listener; one that fell behind misses events, not the stream, because
/// it reads `inspect` on its period anyway.
async fn next_event(
    mut rx: broadcast::Receiver<driver::EventsResponse>,
) -> Option<(
    Result<driver::EventsResponse, ConnectError>,
    broadcast::Receiver<driver::EventsResponse>,
)> {
    loop {
        match rx.recv().await {
            Ok(e) => return Some((Ok(e), rx)),
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

fn grace_of(d: Option<&buffa_types::google::protobuf::Duration>) -> Duration {
    d.map_or(DEFAULT_GRACE, |d| {
        let secs = u64::try_from(d.seconds).unwrap_or(0);
        Duration::from_secs(secs) + Duration::from_nanos(u64::try_from(d.nanos).unwrap_or(0))
    })
}

impl Driver {
    /// A driver over `containerd`, mounting only what `policy` allows; the task that forwards
    /// containerd's events runs until the driver is dropped.
    #[must_use]
    pub fn new(containerd: Containerd, policy: Policy) -> (Self, tokio::task::JoinHandle<()>) {
        let (events, _) = broadcast::channel(EVENT_BACKLOG);
        let watcher = {
            let (c, tx) = (containerd.clone(), events.clone());
            tokio::spawn(async move { c.watch(tx).await })
        };
        (Self { containerd, policy, events }, watcher)
    }
}

#[expect(refining_impl_trait, reason = "async fn handlers for the generated trait")]
impl driver::DriverService for Driver {
    async fn describe(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, driver::DescribeRequest>,
    ) -> ServiceResult<driver::DescribeResponse> {
        let healthy = self.containerd.ping().await;
        Response::ok(driver::DescribeResponse { info: info(healthy).into(), ..Default::default() })
    }

    fn prepare(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::PrepareRequest>,
    ) -> impl Future<Output = ServiceResult<driver::PrepareResponse>> + Send {
        use driver::prepare_response::Result as R;
        let r = request.to_owned_message();
        let intent = r.intent.as_option().cloned().unwrap_or_default();
        let result = match prepare(&r.workload, &intent, &r.recipe_manifest) {
            Ok(spec) => R::Prepared(Box::new(spec)),
            Err(rejection) => R::Rejection(Box::new(rejection)),
        };
        ready(Response::ok(driver::PrepareResponse { result: Some(result), ..Default::default() }))
    }

    async fn create(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::CreateRequest>,
    ) -> ServiceResult<driver::CreateResponse> {
        let spec = request.to_owned_message().spec.as_option().cloned().unwrap_or_default();
        let valid = validate(spec, &self.policy).map_err(|v| status(v.into()))?;
        let runtime_id = self.containerd.create(&valid, &self.policy).await.map_err(status)?;
        Response::ok(driver::CreateResponse { runtime_id, ..Default::default() })
    }

    async fn start(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::StartRequest>,
    ) -> ServiceResult<driver::StartResponse> {
        self.containerd.start(request.runtime_id).await.map_err(status)?;
        Response::ok(driver::StartResponse::default())
    }

    async fn stop(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::StopRequest>,
    ) -> ServiceResult<driver::StopResponse> {
        let r = request.to_owned_message();
        let signal = signal_number(&r.signal).map_err(status)?;
        let grace = grace_of(r.grace.as_option());
        self.containerd.stop(&r.runtime_id, signal, grace).await.map_err(status)?;
        Response::ok(driver::StopResponse::default())
    }

    async fn kill(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::KillRequest>,
    ) -> ServiceResult<driver::KillResponse> {
        self.containerd.kill(request.runtime_id).await.map_err(status)?;
        Response::ok(driver::KillResponse::default())
    }

    async fn destroy(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::DestroyRequest>,
    ) -> ServiceResult<driver::DestroyResponse> {
        self.containerd.destroy(request.runtime_id).await.map_err(status)?;
        Response::ok(driver::DestroyResponse::default())
    }

    async fn recover(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, driver::RecoverRequest>,
    ) -> ServiceResult<driver::RecoverResponse> {
        let objects = self.containerd.recover().await.map_err(status)?;
        Response::ok(driver::RecoverResponse { objects, ..Default::default() })
    }

    async fn inspect(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::InspectRequest>,
    ) -> ServiceResult<driver::InspectResponse> {
        let s = self.containerd.inspect(request.runtime_id).await.map_err(status)?;
        Response::ok(driver::InspectResponse { status: s.into(), ..Default::default() })
    }

    async fn stats(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::StatsRequest>,
    ) -> ServiceResult<driver::StatsResponse> {
        let id = request.runtime_id;
        self.containerd.inspect(id).await.map_err(status)?;
        let u = self.containerd.usage(id).map_err(status)?;
        let usage = pb::ResourceUsage {
            cpu_millis: u.cpu_millis,
            memory_bytes: u.memory_bytes,
            pids: u.pids,
            ..Default::default()
        };
        Response::ok(driver::StatsResponse { usage: usage.into(), ..Default::default() })
    }

    fn events(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, driver::EventsRequest>,
    ) -> impl Future<Output = ServiceResult<ServiceStream<driver::EventsResponse>>> + Send {
        let out = futures_util::stream::unfold(self.events.subscribe(), next_event);
        ready(Response::stream_ok(out))
    }

    async fn signal(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::SignalRequest>,
    ) -> ServiceResult<driver::SignalResponse> {
        let number = signal_number(request.signal).map_err(status)?;
        self.containerd.signal(request.runtime_id, number).await.map_err(status)?;
        Response::ok(driver::SignalResponse::default())
    }
}
