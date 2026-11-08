// SPDX-License-Identifier: Apache-2.0

//! A test extension, the one the end-to-end tests run in a container on a real node: it serves the
//! DNS provider its installation provides, answering pings and refusing every other call, and
//! follows every subscription its installation holds, printing each event to its console. It
//! reaches the control plane however the SDK finds it does, through its node's relay inside a
//! container.
//!
//! The provider's name is `KORPIS_TEST_PROVIDER`, `test` without it. Run as `test-extension
//! --health <protocol>`, it serves a health provider of that query protocol instead, the one the
//! end-to-end tests of query probes judge a workload by: it has the node send `ping`, and the
//! workload is healthy if what came back starts with `pong`. Run as `test-extension --router`, it
//! serves an address router of that name instead, the one the end-to-end test of a migration routes
//! a cluster pool's address with: it routes every address where it is asked to, as a network would,
//! and prints `route <address> <node>` for each. Run as `test-extension --actions`, it serves the
//! actions its document declares under the extension's name: `peek` reads the workload `workload`
//! for its invoker and answers with its name, and with `other`, a workload's identifier, tries to
//! read that one too and says whether it could, which is how the tests show an invocation acts
//! only on what its invoker named.

use korpis_extension::action::{Actions, Answer, Invoker, for_invocation};
use korpis_extension::address::{AddressRouter, Router};
use korpis_extension::consumer::{Consumer, EventHandler};
use korpis_extension::health::{Failure, Health, HealthProvider, send, verdict};
use korpis_extension::provider::{Provider, Registration, Unsupported};
use korpis_extension::{Connection, IdentitySocket};
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::Duration;

/// Writes `line` to the console, which is what the tests read.
fn say(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stdout().lock(), "{line}");
}

/// Prints each event: `event <kind> <id>`.
struct Print;

impl EventHandler for Print {
    fn event(&self, event: pb::Event) -> impl Future<Output = Result<(), String>> + Send {
        say(&format!("event {} {}", event.kind, event.id));
        std::future::ready(Ok(()))
    }
}

/// The `ping`/`pong` query.
struct Echo;

impl HealthProvider for Echo {
    fn step(
        &self,
        step: pb::HealthStep,
    ) -> impl Future<Output = Result<pb::HealthNext, Failure>> + Send {
        let next = match step.rounds.last() {
            None => send(b"ping\n", 16, Duration::from_secs(2)),
            Some(r) => verdict(r.received.starts_with(b"pong"), "it answers pong, or does not"),
        };
        std::future::ready(Ok(next))
    }
}

/// A network that routes every address where it is asked to.
struct Routes;

impl AddressRouter for Routes {
    fn route(
        &self,
        r: pb::RouteAddress,
    ) -> impl Future<Output = Result<(), korpis_extension::address::Failure>> + Send {
        say(&format!("route {} {}", r.address, r.node));
        std::future::ready(Ok(()))
    }
}

/// The `peek` action, reading workloads for its invoker.
struct Peek(Arc<Connection<IdentitySocket>>);

impl Peek {
    async fn read(&self, invoked: &pb::InvokeAction, id: &str) -> Result<String, String> {
        let (http, config) = for_invocation(&self.0, invoked).await.map_err(|e| e.to_string())?;
        let client = pb::WorkloadServiceClient::new(http, config);
        let get = pb::GetWorkloadRequest { id: id.to_owned(), ..Default::default() };
        let got = client.get_workload(get).await.map_err(|e| e.to_string())?.into_owned();
        let workload = got.status.as_option().and_then(|s| s.workload.as_option());
        Ok(workload.map(|w| w.name.clone()).unwrap_or_default())
    }
}

impl Actions for Peek {
    async fn invoke(
        &self,
        invoked: pb::InvokeAction,
    ) -> Result<Answer, korpis_extension::provider::Failure> {
        use korpis_proto::korpis::v1::__buffa::oneof::action_value::Value as V;
        let fail = |m: String| korpis_extension::provider::Failure {
            reason: "test.peek".into(),
            message: m,
            retryable: false,
        };
        let text = |k: &str| match invoked.arguments.get(k).and_then(|v| v.value.clone()) {
            Some(V::Workload(w) | V::Text(w)) => Some(w),
            _ => None,
        };
        let workload = text("workload").ok_or_else(|| fail("no workload".into()))?;
        let name = self.read(&invoked, &workload).await.map_err(fail)?;
        let mut message = format!("peeked {name} for {}", invoked.invoker);
        if let Some(other) = text("other") {
            let seen = self.read(&invoked, &other).await;
            message.push_str(if seen.is_ok() { "; other read" } else { "; other refused" });
        }
        say(&message);
        Ok(Answer { message, data: String::new() })
    }
}

/// Serves the provider `kind` named `name` through `connection`, answering with `handler`.
fn serve<H: korpis_extension::provider::Handler>(
    connection: &Arc<Connection<IdentitySocket>>,
    (kind, name): (pb::ProviderType, String),
    handler: H,
) {
    let registration = Registration {
        kind,
        name,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        concurrency: 4,
        sign_in: false,
        fencing: pb::Fencing::FENCING_UNSPECIFIED,
    };
    let provider = Arc::new(Provider::new(Arc::clone(connection), registration, handler));
    tokio::spawn(async move { provider.run().await });
}

#[tokio::main]
async fn main() {
    let connection = Arc::new(Connection::new(IdentitySocket::default()));
    let args: Vec<String> = std::env::args().collect();
    let health = args.iter().position(|a| a == "--health").and_then(|i| args.get(i + 1));
    if let Some(protocol) = health {
        let kind = pb::ProviderType::PROVIDER_TYPE_HEALTH;
        serve(&connection, (kind, protocol.clone()), Health(Echo));
    } else {
        let name = std::env::var("KORPIS_TEST_PROVIDER").unwrap_or_else(|_| "test".to_owned());
        if args.iter().any(|a| a == "--actions") {
            let kind = pb::ProviderType::PROVIDER_TYPE_ACTION;
            serve(&connection, (kind, name), Invoker(Peek(Arc::clone(&connection))));
        } else if args.iter().any(|a| a == "--router") {
            serve(
                &connection,
                (pb::ProviderType::PROVIDER_TYPE_ADDRESS_ROUTER, name),
                Router(Routes),
            );
        } else {
            serve(&connection, (pb::ProviderType::PROVIDER_TYPE_DNS, name), Unsupported);
        }
    }
    let consumer = Arc::new(Consumer::new(connection, Print));
    // The node may not hold the workload's lease yet; the socket says to ask again.
    let subscriptions = loop {
        match consumer.subscriptions().await {
            Ok(s) => break s,
            Err(e) => {
                say(&format!("subscriptions: {e}"));
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    };
    say(&format!("following {}", subscriptions.len()));
    let mut following = Vec::new();
    for s in subscriptions {
        let consumer = Arc::clone(&consumer);
        following.push(tokio::spawn(async move { consumer.run(&s).await }));
    }
    for f in following {
        // A consumer returns only when it is refused for good.
        if let Ok(e) = f.await {
            say(&format!("a subscription stopped: {e}"));
        }
    }
    std::future::pending::<()>().await;
}
