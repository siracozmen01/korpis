// SPDX-License-Identifier: AGPL-3.0-or-later

//! The runtime driver, as the supervisor uses it: the lifecycle of §4.2 of docs/04-runtimes.md,
//! spoken in the driver protocol's own messages (`korpis.driver.v1`), so the port and the wire
//! a driver process serves cannot drift apart. A driver is a separate process (§3 of
//! docs/04-runtimes.md); an adapter over its socket implements this, and so does the in-process
//! [`memory`] runtime that tests and a driverless trial run use.
//!
//! Liveness is read with `inspect`, polled by the reconciler, which is the level-triggered
//! reading anyway (§4.1 of docs/02-architecture.md); `changes`, the protocol's `Events`, only
//! makes it look sooner, so a crash is seen when it happens. Of the capability-gated verbs,
//! console input and resizing are here (§6 of docs/14-streams.md), and exec, which an `exec`
//! probe runs (§5 of docs/04-runtimes.md); resource resizing is not yet (§4 of
//! docs/20-roadmap.md).

pub mod drivers;
pub mod memory;
pub mod process;

use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::future::Future;
use std::time::Duration;
use tokio::sync::broadcast;

/// What `prepare` answers: a spec the driver will build, or why it will not.
#[derive(Debug, Clone, PartialEq)]
pub enum Prepared {
    /// Resolved and validated; nothing was changed.
    Spec(Box<driver::PreparedSpec>),
    /// This driver on this node cannot satisfy the intent, and says why.
    Rejected(driver::Rejection),
}

/// Why a driver call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    /// No runtime object has that identifier.
    #[error("no runtime object {0}")]
    NotFound(String),
    /// The driver could not be reached.
    #[error("the driver is unavailable: {0}")]
    Unavailable(String),
    /// The driver tried and failed.
    #[error("{0}")]
    Failed(String),
}

/// A result from a driver.
pub type RuntimeResult<T> = Result<T, RuntimeError>;

/// How a command run inside a runtime object ended (§4.2 of docs/04-runtimes.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Executed {
    /// Its exit status; 128 and the signal's number for one a signal ended.
    pub status: u32,
    /// Killed for outliving its timeout.
    pub timed_out: bool,
    /// What it wrote, stdout and stderr together, at most the limit asked for.
    pub output: Vec<u8>,
}

/// Who sent a console input, and on which data-path session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sender<'a> {
    /// The session token's subject.
    pub by: &'a str,
    /// The session.
    pub session: &'a str,
}

/// A runtime driver.
pub trait Runtime: Send + Sync + 'static {
    /// Its declared capabilities: asked, never inferred.
    fn describe(&self) -> impl Future<Output = RuntimeResult<pb::DriverInfo>> + Send;

    /// Every driver it reaches, each as it declares itself: one, unless it is [`drivers`]'s.
    fn drivers(&self) -> impl Future<Output = RuntimeResult<Vec<pb::DriverInfo>>> + Send {
        async { Ok(vec![self.describe().await?]) }
    }

    /// Validates and resolves `intent` for `workload`, changing nothing.
    fn prepare(
        &self,
        workload: &str,
        intent: &pb::Intent,
        manifest: &[u8],
    ) -> impl Future<Output = RuntimeResult<Prepared>> + Send;

    /// Allocates what `spec` needs, executing nothing; idempotent by workload and intent. The
    /// runtime object's identifier.
    fn create(
        &self,
        spec: &driver::PreparedSpec,
    ) -> impl Future<Output = RuntimeResult<String>> + Send;

    /// Starts it.
    fn start(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send;

    /// Stops it with `signal` (empty for the driver's graceful default), killing it after
    /// `grace`.
    fn stop(
        &self,
        id: &str,
        signal: &str,
        grace: Duration,
    ) -> impl Future<Output = RuntimeResult<()>> + Send;

    /// Kills it.
    fn kill(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send;

    /// Releases every resource it holds.
    fn destroy(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send;

    /// Every runtime object this driver created, with its labels: adoption's input (§4.4 of
    /// docs/02-architecture.md).
    fn recover(&self) -> impl Future<Output = RuntimeResult<Vec<driver::RuntimeObject>>> + Send;

    /// What it is doing now.
    fn inspect(
        &self,
        id: &str,
    ) -> impl Future<Output = RuntimeResult<driver::RuntimeStatus>> + Send;

    /// What it is using now, and its counters since it was made, each absent where the driver
    /// cannot read it (§2 of docs/15-observability.md).
    fn stats(&self, id: &str) -> impl Future<Output = RuntimeResult<driver::StatsResponse>> + Send;

    /// Sends it a driver-declared signal.
    fn signal(&self, id: &str, signal: &str) -> impl Future<Output = RuntimeResult<()>> + Send;

    /// Sends `data` to its interactive console, from the subject and session `from` names, to
    /// be recorded with them.
    fn console_input(
        &self,
        id: &str,
        from: Sender<'_>,
        data: &[u8],
    ) -> impl Future<Output = RuntimeResult<()>> + Send;

    /// Resizes its interactive console's terminal to `(cols, rows)`.
    fn resize_console(
        &self,
        id: &str,
        from: Sender<'_>,
        size: (u16, u16),
    ) -> impl Future<Output = RuntimeResult<()>> + Send;

    /// Runs `command` inside it, in its namespaces as its own user, for at most `timeout`,
    /// keeping at most `output_limit` bytes of what it writes. Capability-gated: a driver that
    /// declares `exec`.
    fn exec(
        &self,
        id: &str,
        command: &[String],
        timeout: Duration,
        output_limit: u32,
    ) -> impl Future<Output = RuntimeResult<Executed>> + Send;

    /// Whether the agent inside its guest answers within `within`: what a `guest_agent` probe
    /// asks. Capability-gated: a driver that declares `guest_agent`. An object that does not run
    /// is an error, not a guest that did not answer.
    fn ping_guest(
        &self,
        id: &str,
        within: Duration,
    ) -> impl Future<Output = RuntimeResult<bool>> + Send;

    /// The runtime objects whose status changes, by identifier, as the driver reports them: a
    /// reason to look sooner, never a reading, which `inspect` is.
    fn changes(&self) -> broadcast::Receiver<String>;
}
