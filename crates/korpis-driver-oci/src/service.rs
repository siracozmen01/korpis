// SPDX-License-Identifier: AGPL-3.0-or-later

//! The driver protocol (`korpis.driver.v1.DriverService`) answered over containerd: each verb
//! of §4.2 of docs/04-runtimes.md mapped onto [`Containerd`], every spec validated again at
//! `Create` whatever `Prepare` said, and every failure given the code the agent branches on.

use crate::containerd::{Containerd, Sender, signal_number};
use crate::describe::info;
use crate::error::DriverError;
use crate::prepare::prepare;
use crate::validate::{Policy, validate};
use connectrpc::{ConnectError, ErrorCode, RequestContext, Response, ServiceRequest};
use connectrpc::{ServiceResult, ServiceStream};
use korpis_driver_kit::exec::{Command, Ran, response};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::future::{Future, ready};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

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

/// A duration of the protocol's; zero where absent.
fn duration_of(d: Option<(i64, i32)>) -> Duration {
    d.map_or(Duration::ZERO, |(seconds, nanos)| {
        let secs = u64::try_from(seconds).unwrap_or(0);
        Duration::from_secs(secs) + Duration::from_nanos(u64::try_from(nanos).unwrap_or(0))
    })
}

/// What a command's run sends while it runs, then how it ended; or, if it could not be run, why.
fn exec_stream(
    ran: mpsc::Receiver<Ran>,
    running: tokio::task::JoinHandle<Result<(), DriverError>>,
) -> impl futures_util::Stream<Item = Result<driver::ExecResponse, ConnectError>> + Send + 'static {
    futures_util::stream::unfold(Some((ran, running)), |state| async move {
        let (mut ran, running) = state?;
        if let Some(r) = ran.recv().await {
            return Some((Ok(response(r)), Some((ran, running))));
        }
        match running.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some((Err(status(e)), None)),
            Err(e) => Some((Err(ConnectError::new(ErrorCode::Internal, e.to_string())), None)),
        }
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
        let counters = self.containerd.counters(id);
        Response::ok(driver::StatsResponse {
            usage: usage.into(),
            counters: counters.into(),
            ..Default::default()
        })
    }

    fn events(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, driver::EventsRequest>,
    ) -> impl Future<Output = ServiceResult<ServiceStream<driver::EventsResponse>>> + Send {
        let out = futures_util::stream::unfold(self.events.subscribe(), next_event);
        ready(Response::stream_ok(out))
    }

    async fn console_input(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::ConsoleInputRequest>,
    ) -> ServiceResult<driver::ConsoleInputResponse> {
        let from = Sender { by: request.by, session: request.session };
        self.containerd
            .console_input(request.runtime_id, from, request.data)
            .await
            .map_err(status)?;
        Response::ok(driver::ConsoleInputResponse::default())
    }

    async fn resize_console(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::ResizeConsoleRequest>,
    ) -> ServiceResult<driver::ResizeConsoleResponse> {
        let from = Sender { by: request.by, session: request.session };
        let size = |n: u32| {
            u16::try_from(n).map_err(|_| {
                status(DriverError::Rejected("a terminal is at most 65535 columns or rows".into()))
            })
        };
        let size = (size(request.cols)?, size(request.rows)?);
        self.containerd.resize_console(request.runtime_id, from, size).await.map_err(status)?;
        Response::ok(driver::ResizeConsoleResponse::default())
    }

    fn exec(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::ExecRequest>,
    ) -> impl Future<Output = ServiceResult<ServiceStream<driver::ExecResponse>>> + Send {
        let argv = request.command.iter().map(|a| (*a).to_owned()).collect();
        let timeout = duration_of(request.timeout.as_option().map(|d| (d.seconds, d.nanos)));
        let checked =
            Command::checked(argv, timeout, request.output_limit).map_err(DriverError::Rejected);
        let (id, containerd) = (request.runtime_id.to_owned(), self.containerd.clone());
        ready(checked.map_err(status).and_then(|c| {
            let (tx, rx) = mpsc::channel(16);
            let running = tokio::spawn(async move { containerd.exec(&id, c, tx).await });
            Response::stream_ok(exec_stream(rx, running))
        }))
    }

    /// Refused: a container has no agent of its own; the driver declares no `guest_agent`.
    fn ping_guest(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, driver::PingGuestRequest>,
    ) -> impl Future<Output = ServiceResult<driver::PingGuestResponse>> + Send {
        let why = "a container has no guest agent; this driver declares no guest_agent";
        ready(Err(ConnectError::new(ErrorCode::FailedPrecondition, why)))
    }

    /// Refused: a container's files are served on the host by the helper's worker; the driver
    /// declares `file_access` `HOST_WORKER`, not `GUEST_WORKER`.
    fn files(
        &self,
        _ctx: RequestContext,
        _requests: connectrpc::InboundStream<driver::FilesRequest>,
    ) -> impl Future<Output = ServiceResult<ServiceStream<driver::FilesResponse>>> + Send {
        let why = "a container's files are served on its host; this driver has no guest worker";
        ready(Err(ConnectError::new(ErrorCode::FailedPrecondition, why)))
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
