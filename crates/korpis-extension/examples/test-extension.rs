// SPDX-License-Identifier: Apache-2.0

//! A test extension, the one the end-to-end tests run in a container on a real node: it serves
//! the DNS provider its installation provides, answering pings and refusing every other call, and
//! follows every subscription its installation holds, printing each event to its console. It
//! reaches the control plane however the SDK finds it does, through its node's relay inside a
//! container (§9.5 of docs/08-identity.md).
//!
//! The provider's name is `KORPIS_TEST_PROVIDER`, `test` without it.

use korpis_extension::consumer::{Consumer, EventHandler};
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

#[tokio::main]
async fn main() {
    let connection = Arc::new(Connection::new(IdentitySocket::default()));
    let name = std::env::var("KORPIS_TEST_PROVIDER").unwrap_or_else(|_| "test".to_owned());
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_DNS,
        name,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        concurrency: 4,
    };
    let provider = Arc::new(Provider::new(Arc::clone(&connection), registration, Unsupported));
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
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
