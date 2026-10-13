// SPDX-License-Identifier: AGPL-3.0-or-later

//! The runtime driver, as the supervisor uses it: the lifecycle of §4.2 of docs/04-runtimes.md,
//! spoken in the driver protocol's own messages (`korpis.driver.v1`), so the port and the wire
//! a driver process serves cannot drift apart. A driver is a separate process (§3 of
//! docs/04-runtimes.md); an adapter over its socket implements this, and so does the in-process
//! [`memory`] runtime that tests and a driverless trial run use.
//!
//! Liveness is read with `inspect`, polled by the reconciler, which is the level-triggered
//! reading anyway (§4.1 of docs/02-architecture.md). The protocol's `Events` stream makes a
//! crash visible sooner and arrives with the oci driver's adapter; the capability-gated verbs
//! (exec, console, resize) arrive with the console and resizing of Phase 1 (§4 of
//! docs/20-roadmap.md).

pub mod memory;

use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::future::Future;
use std::time::Duration;

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

/// A runtime driver.
pub trait Runtime: Send + Sync + 'static {
    /// Its declared capabilities: asked, never inferred.
    fn describe(&self) -> impl Future<Output = RuntimeResult<pb::DriverInfo>> + Send;

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

    /// What it is using.
    fn stats(&self, id: &str) -> impl Future<Output = RuntimeResult<pb::ResourceUsage>> + Send;

    /// Sends it a driver-declared signal.
    fn signal(&self, id: &str, signal: &str) -> impl Future<Output = RuntimeResult<()>> + Send;
}
