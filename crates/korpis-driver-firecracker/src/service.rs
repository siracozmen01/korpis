// SPDX-License-Identifier: AGPL-3.0-or-later

//! The driver protocol (`korpis.driver.v1.DriverService`) answered over [`Machine`]: each verb of
//! §4.2 of docs/04-runtimes.md, and every failure given the code the agent branches on. A
//! console's input reaches the guest; resizing is refused, since nothing carries a terminal's
//! size into a guest until its agent does (§6 of docs/04-runtimes.md).

use crate::describe::info;
use crate::machine::{Machine, MachineError};
use crate::prepare::prepare;
use connectrpc::{ConnectError, ErrorCode, RequestContext, Response, ServiceRequest};
use connectrpc::{ServiceResult, ServiceStream};
use korpis_proto::korpis::driver::v1 as driver;
use std::future::{Future, ready};
use std::time::Duration;
use tokio::sync::broadcast;

/// The grace a stop gets when the request names none.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(10);
/// The longest a guest's agent is given to answer.
const MOST_PING: Duration = Duration::from_secs(60);

/// The driver's service.
#[derive(Debug, Clone)]
pub struct Driver {
    machine: Machine,
}

impl Driver {
    /// The service over `machine`.
    #[must_use]
    pub fn new(machine: Machine) -> Self {
        Self { machine }
    }

    /// Whether this machine can run a guest now: KVM, Firecracker, its jailer, and the kernel.
    fn healthy(&self) -> bool {
        let s = self.machine.settings();
        let kvm = std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok();
        kvm && [&s.firecracker, &s.jailer, &s.kernel, &s.guest, &s.image_build, &s.writer]
            .iter()
            .all(|p| p.is_file())
    }
}

/// A failure as the protocol's error; a refusal is a security event, logged where it happens.
fn status(e: MachineError) -> ConnectError {
    let (code, message) = match e {
        MachineError::Refused(why) => {
            tracing::warn!(%why, "a request was refused");
            (ErrorCode::FailedPrecondition, format!("refused: {why}"))
        }
        MachineError::NotFound(id) => (ErrorCode::NotFound, format!("no runtime object {id}")),
        MachineError::Failed(m) => (ErrorCode::Internal, m),
    };
    ConnectError::new(code, message)
}

/// The next event for a listener; one that fell behind misses events, not the stream.
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

fn no_resize() -> ConnectError {
    let why = "a guest's terminal cannot be resized: nothing carries its size into the guest yet";
    ConnectError::new(ErrorCode::FailedPrecondition, why)
}

#[expect(refining_impl_trait, reason = "async fn handlers for the generated trait")]
impl driver::DriverService for Driver {
    fn describe(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, driver::DescribeRequest>,
    ) -> impl Future<Output = ServiceResult<driver::DescribeResponse>> + Send {
        let info = info(self.healthy());
        ready(Response::ok(driver::DescribeResponse { info: info.into(), ..Default::default() }))
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
        let runtime_id = self.machine.create(spec).await.map_err(status)?;
        Response::ok(driver::CreateResponse { runtime_id, ..Default::default() })
    }

    async fn start(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::StartRequest>,
    ) -> ServiceResult<driver::StartResponse> {
        self.machine.start(request.runtime_id).await.map_err(status)?;
        Response::ok(driver::StartResponse::default())
    }

    async fn stop(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::StopRequest>,
    ) -> ServiceResult<driver::StopResponse> {
        let r = request.to_owned_message();
        let grace = grace_of(r.grace.as_option());
        self.machine.stop(&r.runtime_id, &r.signal, grace).await.map_err(status)?;
        Response::ok(driver::StopResponse::default())
    }

    async fn kill(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::KillRequest>,
    ) -> ServiceResult<driver::KillResponse> {
        self.machine.kill(request.runtime_id).await.map_err(status)?;
        Response::ok(driver::KillResponse::default())
    }

    async fn destroy(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::DestroyRequest>,
    ) -> ServiceResult<driver::DestroyResponse> {
        self.machine.destroy(request.runtime_id).await.map_err(status)?;
        Response::ok(driver::DestroyResponse::default())
    }

    fn recover(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, driver::RecoverRequest>,
    ) -> impl Future<Output = ServiceResult<driver::RecoverResponse>> + Send {
        let objects = self.machine.recover();
        ready(Response::ok(driver::RecoverResponse { objects, ..Default::default() }))
    }

    fn inspect(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::InspectRequest>,
    ) -> impl Future<Output = ServiceResult<driver::InspectResponse>> + Send {
        let answer = self.machine.inspect(request.runtime_id).map_err(status).and_then(|s| {
            Response::ok(driver::InspectResponse { status: s.into(), ..Default::default() })
        });
        ready(answer)
    }

    fn stats(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::StatsRequest>,
    ) -> impl Future<Output = ServiceResult<driver::StatsResponse>> + Send {
        ready(self.machine.stats(request.runtime_id).map_err(status).and_then(Response::ok))
    }

    fn events(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, driver::EventsRequest>,
    ) -> impl Future<Output = ServiceResult<ServiceStream<driver::EventsResponse>>> + Send {
        let out = futures_util::stream::unfold(self.machine.events(), next_event);
        ready(Response::stream_ok(out))
    }

    async fn console_input(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::ConsoleInputRequest>,
    ) -> ServiceResult<driver::ConsoleInputResponse> {
        let from = (request.by, request.session);
        let sent = self.machine.console_input(request.runtime_id, from, request.data).await;
        sent.map_err(status)?;
        Response::ok(driver::ConsoleInputResponse::default())
    }

    fn resize_console(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, driver::ResizeConsoleRequest>,
    ) -> impl Future<Output = ServiceResult<driver::ResizeConsoleResponse>> + Send {
        ready(Err(no_resize()))
    }

    async fn signal(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::SignalRequest>,
    ) -> ServiceResult<driver::SignalResponse> {
        self.machine.signal(request.runtime_id, request.signal).await.map_err(status)?;
        Response::ok(driver::SignalResponse::default())
    }

    async fn ping_guest(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, driver::PingGuestRequest>,
    ) -> ServiceResult<driver::PingGuestResponse> {
        let r = request.to_owned_message();
        let within = r.timeout.as_option().map_or(Duration::ZERO, |d| grace_of(Some(d)));
        if within.is_zero() || within > MOST_PING {
            let why = "a guest's agent is given more than nothing and at most a minute";
            return Err(ConnectError::new(ErrorCode::InvalidArgument, why));
        }
        let pinged = self.machine.ping_guest(&r.runtime_id, within).await.map_err(status)?;
        let detail = pinged.as_ref().err().cloned().unwrap_or_default();
        let answer =
            driver::PingGuestResponse { answered: pinged.is_ok(), detail, ..Default::default() };
        Response::ok(answer)
    }

    /// Refused: nothing runs a command in a guest until its agent is reached over vsock, and the
    /// driver declares no `exec` meanwhile (§5 of docs/04-runtimes.md).
    fn exec(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, driver::ExecRequest>,
    ) -> impl Future<Output = ServiceResult<ServiceStream<driver::ExecResponse>>> + Send {
        let why = "this driver runs no command inside a guest; it declares no exec";
        ready(Err(ConnectError::new(ErrorCode::FailedPrecondition, why)))
    }
}
