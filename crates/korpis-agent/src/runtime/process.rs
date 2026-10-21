// SPDX-License-Identifier: AGPL-3.0-or-later

//! A driver process, as a [`Runtime`]: the driver protocol (`korpis.driver.v1`) over the
//! driver's Unix socket, gRPC on HTTP/2 (§3 of docs/04-runtimes.md). Every call has a deadline,
//! so a driver that hangs costs the reconciler one pass rather than the node; `Create` gets the
//! longest, because it may import and unpack an image. The driver's `Events` stream is held
//! open for as long as the runtime lives, reopened when it ends, and each event wakes the
//! reconciler for the object it names.

use super::{Prepared, Runtime, RuntimeError, RuntimeResult, Sender};
use connectrpc::client::{CallOptions, ClientConfig, Http2Connection, SharedHttp2Connection};
use connectrpc::{ConnectError, ErrorCode, Protocol};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// The deadline of a call that should answer at once.
const QUICK: Duration = Duration::from_secs(30);
/// The deadline of a `Create`, which may import and unpack an image.
const CREATE: Duration = Duration::from_secs(900);
/// How long a `Stop` may take beyond its grace, for the kill that follows it.
const STOP_MARGIN: Duration = Duration::from_secs(30);
/// How long to wait before reopening the events stream.
const REOPEN: Duration = Duration::from_secs(1);
/// How many changes a slow reconciler may fall behind by.
const CHANGES: usize = 256;

type Client = driver::DriverServiceClient<SharedHttp2Connection>;

/// Stops the events task when the last clone of the runtime goes.
#[derive(Debug)]
struct Watcher(tokio::task::JoinHandle<()>);

impl Drop for Watcher {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A driver process reached over its socket; clones share the connection.
#[derive(Clone)]
pub struct DriverRuntime {
    client: Client,
    changes: broadcast::Sender<String>,
    _watcher: Arc<Watcher>,
}

impl std::fmt::Debug for DriverRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverRuntime").finish_non_exhaustive()
    }
}

fn error(e: ConnectError) -> RuntimeError {
    let message = e.message.unwrap_or_else(|| format!("{:?}", e.code));
    match e.code {
        ErrorCode::NotFound => RuntimeError::NotFound(message),
        ErrorCode::Unavailable | ErrorCode::DeadlineExceeded => RuntimeError::Unavailable(message),
        _ => RuntimeError::Failed(message),
    }
}

fn within(timeout: Duration) -> CallOptions {
    CallOptions::default().with_timeout(timeout)
}

/// Forwards the driver's events, reopening the stream whenever it ends.
async fn watch(client: Client, out: broadcast::Sender<String>) {
    loop {
        match client.events(driver::EventsRequest::default()).await {
            Ok(mut stream) => {
                while let Ok(Some(e)) = stream.message::<driver::EventsResponse>().await {
                    let _ = out.send(e.to_owned_message().runtime_id);
                }
            }
            Err(e) => tracing::debug!(error = %e, "the driver's events are unavailable"),
        }
        tokio::time::sleep(REOPEN).await;
    }
}

impl DriverRuntime {
    /// The driver serving on `socket`; it is dialled when first used, and its events are
    /// followed from now on. Must be called inside a Tokio runtime.
    #[must_use]
    pub fn connect(socket: &Path) -> Self {
        let authority = http::Uri::from_static("http://localhost");
        let conn = Http2Connection::lazy_unix(socket, authority.clone()).shared(64);
        let config = ClientConfig::new(authority).with_protocol(Protocol::Grpc);
        let client = driver::DriverServiceClient::new(conn, config);
        let (changes, _) = broadcast::channel(CHANGES);
        let watcher = Watcher(tokio::spawn(watch(client.clone(), changes.clone())));
        Self { client, changes, _watcher: Arc::new(watcher) }
    }

    async fn prepare_call(
        &self,
        workload: &str,
        intent: &pb::Intent,
        manifest: &[u8],
    ) -> RuntimeResult<Prepared> {
        use driver::prepare_response::Result as R;
        let request = driver::PrepareRequest {
            workload: workload.to_owned(),
            intent: intent.clone().into(),
            recipe_manifest: manifest.to_vec(),
            ..Default::default()
        };
        let r = self.client.prepare_with_options(request, within(QUICK)).await.map_err(error)?;
        match r.into_owned().result {
            Some(R::Prepared(spec)) => Ok(Prepared::Spec(spec)),
            Some(R::Rejection(r)) => Ok(Prepared::Rejected(*r)),
            None => Err(RuntimeError::Failed("the driver answered Prepare with nothing".into())),
        }
    }

    async fn stop_call(&self, id: &str, signal: &str, grace: Duration) -> RuntimeResult<()> {
        let request = driver::StopRequest {
            runtime_id: id.to_owned(),
            signal: signal.to_owned(),
            grace: buffa_types::google::protobuf::Duration {
                seconds: i64::try_from(grace.as_secs()).unwrap_or(i64::MAX),
                nanos: i32::try_from(grace.subsec_nanos()).unwrap_or(0),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        let options = within(grace.saturating_add(STOP_MARGIN));
        self.client.stop_with_options(request, options).await.map_err(error)?;
        Ok(())
    }
}

mod verbs;

impl Runtime for DriverRuntime {
    fn describe(&self) -> impl Future<Output = RuntimeResult<pb::DriverInfo>> + Send {
        self.describe_call()
    }

    fn prepare(
        &self,
        workload: &str,
        intent: &pb::Intent,
        manifest: &[u8],
    ) -> impl Future<Output = RuntimeResult<Prepared>> + Send {
        self.prepare_call(workload, intent, manifest)
    }

    fn create(
        &self,
        spec: &driver::PreparedSpec,
    ) -> impl Future<Output = RuntimeResult<String>> + Send {
        self.create_call(spec)
    }

    fn start(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.start_call(id)
    }

    fn stop(
        &self,
        id: &str,
        signal: &str,
        grace: Duration,
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.stop_call(id, signal, grace)
    }

    fn kill(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.kill_call(id)
    }

    fn destroy(&self, id: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.destroy_call(id)
    }

    fn recover(&self) -> impl Future<Output = RuntimeResult<Vec<driver::RuntimeObject>>> + Send {
        self.recover_call()
    }

    fn inspect(
        &self,
        id: &str,
    ) -> impl Future<Output = RuntimeResult<driver::RuntimeStatus>> + Send {
        self.inspect_call(id)
    }

    fn stats(&self, id: &str) -> impl Future<Output = RuntimeResult<driver::StatsResponse>> + Send {
        self.stats_call(id)
    }

    fn signal(&self, id: &str, signal: &str) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.signal_call(id, signal)
    }

    fn console_input(
        &self,
        id: &str,
        from: Sender<'_>,
        data: &[u8],
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.input_call(id, from, data)
    }

    fn resize_console(
        &self,
        id: &str,
        from: Sender<'_>,
        size: (u16, u16),
    ) -> impl Future<Output = RuntimeResult<()>> + Send {
        self.resize_call(id, from, size)
    }

    fn changes(&self) -> broadcast::Receiver<String> {
        self.changes.subscribe()
    }
}
