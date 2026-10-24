// SPDX-License-Identifier: Apache-2.0

//! The driver, reached as the node reaches it: `korpis.driver.v1`, gRPC on HTTP/2 over its Unix
//! socket (§3 of docs/04-runtimes.md). Every call has a deadline, so a driver that hangs fails
//! the case it hangs in rather than the run. The driver's events are followed for the whole run
//! and kept, so a case can ask whether one was sent for its object.

use connectrpc::client::{CallOptions, ClientConfig, Http2Connection, SharedHttp2Connection};
use connectrpc::{ConnectError, ErrorCode, Protocol};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The deadline of a call that should answer at once.
const QUICK: Duration = Duration::from_secs(30);
/// The deadline of a `Create`, which may import and unpack an image.
const CREATE: Duration = Duration::from_secs(600);
/// How long a `Stop` may take beyond its grace, for the kill that follows it.
const STOP_MARGIN: Duration = Duration::from_secs(30);

type Client = driver::DriverServiceClient<SharedHttp2Connection>;

/// A call the driver refused or could not answer: its code and what it said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The status code.
    pub code: ErrorCode,
    /// What came with it.
    pub message: String,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl From<ConnectError> for Refused {
    fn from(e: ConnectError) -> Self {
        Self { code: e.code, message: e.message.unwrap_or_default() }
    }
}

/// What a call answered.
pub type Answer<T> = Result<T, Refused>;

/// An event the driver sent: the object it named and the status it said.
pub type Event = (String, driver::RuntimeStatus);

fn within(timeout: Duration) -> CallOptions {
    CallOptions::default().with_timeout(timeout)
}

/// A protocol duration of `d`.
#[must_use]
pub fn duration(d: Duration) -> buffa_types::google::protobuf::Duration {
    buffa_types::google::protobuf::Duration {
        seconds: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(d.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    }
}

/// Ends the task following events when the last clone of the client goes.
#[derive(Debug)]
struct Watcher(tokio::task::JoinHandle<()>);

impl Drop for Watcher {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The driver under test.
#[derive(Clone)]
pub struct Driver {
    client: Client,
    events: Arc<Mutex<Vec<Event>>>,
    _watcher: Arc<Watcher>,
}

impl fmt::Debug for Driver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Driver").finish_non_exhaustive()
    }
}

/// Keeps `e`.
fn keep(kept: &Mutex<Vec<Event>>, e: driver::EventsResponse) {
    if let Ok(mut kept) = kept.lock() {
        kept.push((e.runtime_id, e.status.into_option().unwrap_or_default()));
    }
}

/// Keeps every event the driver sends, reopening the stream whenever it ends.
async fn follow(client: Client, kept: Arc<Mutex<Vec<Event>>>) {
    loop {
        if let Ok(mut stream) = client.events(driver::EventsRequest::default()).await {
            while let Ok(Some(e)) = stream.message::<driver::EventsResponse>().await {
                keep(&kept, e.to_owned_message());
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

impl Driver {
    /// The driver serving on `socket`, its events followed from now on, until the last clone of
    /// it goes. Must be called inside a Tokio runtime.
    #[must_use]
    pub fn connect(socket: &Path) -> Self {
        let authority = http::Uri::from_static("http://localhost");
        let conn = Http2Connection::lazy_unix(socket, authority.clone()).shared(16);
        let config = ClientConfig::new(authority).with_protocol(Protocol::Grpc);
        let client = driver::DriverServiceClient::new(conn, config);
        let events = Arc::new(Mutex::new(Vec::new()));
        let watcher = Watcher(tokio::spawn(follow(client.clone(), events.clone())));
        Self { client, events, _watcher: Arc::new(watcher) }
    }

    /// Every event the driver has sent for `id` so far.
    #[must_use]
    pub fn events_for(&self, id: &str) -> Vec<driver::RuntimeStatus> {
        let kept = self.events.lock().map(|k| k.clone()).unwrap_or_default();
        kept.into_iter().filter(|(of, _)| of == id).map(|(_, s)| s).collect()
    }

    /// `Describe`.
    pub async fn describe(&self) -> Answer<pb::DriverInfo> {
        let r =
            self.client.describe_with_options(driver::DescribeRequest::default(), within(QUICK));
        Ok(r.await?.into_owned().info.into_option().unwrap_or_default())
    }

    /// `Prepare` of `intent` for `workload` from the recipe document `recipe`.
    pub async fn prepare(
        &self,
        workload: &str,
        intent: &pb::Intent,
        recipe: &[u8],
    ) -> Answer<driver::PrepareResponse> {
        let request = driver::PrepareRequest {
            workload: workload.to_owned(),
            intent: intent.clone().into(),
            recipe_manifest: recipe.to_vec(),
            ..Default::default()
        };
        Ok(self.client.prepare_with_options(request, within(QUICK)).await?.into_owned())
    }

    /// `Create` of `spec`: the object's id.
    pub async fn create(&self, spec: &driver::PreparedSpec) -> Answer<String> {
        let request = driver::CreateRequest { spec: spec.clone().into(), ..Default::default() };
        Ok(self.client.create_with_options(request, within(CREATE)).await?.into_owned().runtime_id)
    }

    /// `Start`.
    pub async fn start(&self, id: &str) -> Answer<()> {
        let request = driver::StartRequest { runtime_id: id.to_owned(), ..Default::default() };
        self.client.start_with_options(request, within(QUICK)).await?;
        Ok(())
    }

    /// `Stop` with the driver's graceful default and `grace`.
    pub async fn stop(&self, id: &str, grace: Duration) -> Answer<()> {
        let request = driver::StopRequest {
            runtime_id: id.to_owned(),
            grace: duration(grace).into(),
            ..Default::default()
        };
        let options = within(grace.saturating_add(STOP_MARGIN));
        self.client.stop_with_options(request, options).await?;
        Ok(())
    }

    /// `Kill`.
    pub async fn kill(&self, id: &str) -> Answer<()> {
        let request = driver::KillRequest { runtime_id: id.to_owned(), ..Default::default() };
        self.client.kill_with_options(request, within(QUICK)).await?;
        Ok(())
    }

    /// `Destroy`.
    pub async fn destroy(&self, id: &str) -> Answer<()> {
        let request = driver::DestroyRequest { runtime_id: id.to_owned(), ..Default::default() };
        self.client.destroy_with_options(request, within(QUICK)).await?;
        Ok(())
    }

    /// `Recover`.
    pub async fn recover(&self) -> Answer<Vec<driver::RuntimeObject>> {
        let r = self.client.recover_with_options(driver::RecoverRequest::default(), within(QUICK));
        Ok(r.await?.into_owned().objects)
    }

    /// `Inspect`.
    pub async fn inspect(&self, id: &str) -> Answer<driver::RuntimeStatus> {
        let request = driver::InspectRequest { runtime_id: id.to_owned(), ..Default::default() };
        let r = self.client.inspect_with_options(request, within(QUICK)).await?;
        Ok(r.into_owned().status.into_option().unwrap_or_default())
    }

    /// `Stats`.
    pub async fn stats(&self, id: &str) -> Answer<driver::StatsResponse> {
        let request = driver::StatsRequest { runtime_id: id.to_owned(), ..Default::default() };
        Ok(self.client.stats_with_options(request, within(QUICK)).await?.into_owned())
    }

    /// `Signal` of `signal`.
    pub async fn signal(&self, id: &str, signal: &str) -> Answer<()> {
        let request = driver::SignalRequest {
            runtime_id: id.to_owned(),
            signal: signal.to_owned(),
            ..Default::default()
        };
        self.client.signal_with_options(request, within(QUICK)).await?;
        Ok(())
    }

    /// `Exec` of `command` in `id`, for at most `timeout`, keeping at most `limit` bytes of what
    /// it writes: what it wrote, and how it ended.
    pub async fn exec(
        &self,
        id: &str,
        command: &[&str],
        (timeout, limit): (Duration, u32),
    ) -> Answer<(Vec<u8>, driver::ExecExit)> {
        use driver::__buffa::oneof::exec_response::Event as E;
        let request = driver::ExecRequest {
            runtime_id: id.to_owned(),
            command: command.iter().map(|c| (*c).to_owned()).collect(),
            timeout: duration(timeout).into(),
            output_limit: limit,
            ..Default::default()
        };
        let options = within(timeout.saturating_add(QUICK));
        let mut stream = self.client.exec_with_options(request, options).await?;
        let mut output = Vec::new();
        while let Some(m) = stream.message::<driver::ExecResponse>().await? {
            match m.to_owned_message().event {
                Some(E::Output(data)) => output.extend(data),
                Some(E::Exit(exit)) => return Ok((output, *exit)),
                None => {}
            }
        }
        let message = "the stream ended without the command's exit".to_owned();
        Err(Refused { code: ErrorCode::DataLoss, message })
    }
}
