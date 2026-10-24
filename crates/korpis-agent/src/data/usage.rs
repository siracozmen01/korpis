// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a workload uses now, over the data path: the handler of a `metering.read` session over
//! the workload (§2 of docs/15-observability.md). Its params are empty. The node sends a
//! `UsageNow` at each of its meter's readings, every ten seconds, from the readings the meter
//! takes anyway, so a session watching costs the node no reading of its own; until the meter has
//! read the workload it sends nothing. The session ends when the client is done or its token
//! does.
//!
//! It is not journaled, as a console read is (§8 of docs/17-security.md): what it shows is what
//! the control plane already answers `ListUsage` with under the same action, an interval at a
//! time, and nothing the workload wrote.

use super::handler::{Ended, Handler, Io, Opened, Serving};
use crate::meter::now::{Now, Readings, usage_now};
use buffa::Message as _;
use tokio::sync::watch;

/// The action.
pub const ACTION: &str = "metering.read";

/// Serves `metering.read` sessions from the meter's readings.
#[derive(Debug, Clone)]
pub struct UsageRead {
    readings: watch::Receiver<Readings>,
}

impl UsageRead {
    /// Serves what `readings` publishes.
    #[must_use]
    pub const fn new(readings: watch::Receiver<Readings>) -> Self {
        Self { readings }
    }
}

impl Handler for UsageRead {
    fn serve(&self, opened: Opened, io: Io) -> Serving {
        Box::pin(serve(self.readings.clone(), opened, io))
    }
}

fn ended(code: &str, message: &str) -> Ended {
    Ended { code: code.to_owned(), message: message.to_owned() }
}

async fn serve(
    mut readings: watch::Receiver<Readings>,
    opened: Opened,
    mut io: Io,
) -> Result<(), Ended> {
    if !opened.params.is_empty() {
        return Err(ended("invalid", "a usage session takes no params"));
    }
    let mut before: Option<Now> = None;
    loop {
        let latest = readings.borrow_and_update().get(&opened.workload).cloned();
        if let Some(now) = latest.filter(|n| before.as_ref().is_none_or(|b| b.at != n.at)) {
            let message = usage_now(before.as_ref(), &now);
            if io.outgoing.send(message.encode_to_vec()).await.is_err() {
                return Ok(());
            }
            before = Some(now);
        }
        tokio::select! {
            changed = readings.changed() => if changed.is_err() {
                return Err(ended("unavailable", "the node's meter stopped"));
            },
            got = io.incoming.recv() => if got.is_none() { return Ok(()) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_proto::korpis::agent::v1 as agent;
    use korpis_proto::korpis::v1 as pb;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};
    use tokio::sync::mpsc;

    fn opened(params: Vec<u8>) -> Opened {
        Opened {
            session: "s".into(),
            workload: "wl_a".into(),
            action: ACTION.into(),
            params,
            subject: "user:x".into(),
            token: String::new(),
            chain: Vec::new(),
            expires_at: SystemTime::now() + Duration::from_secs(60),
            stream_range: None,
        }
    }

    fn reading(workload: &str, secs: u64, cpu: u64) -> Readings {
        let now = Now {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            counters: agent::MeterCounters { cpu_usec: Some(cpu), ..Default::default() },
            memory_bytes: Some(1 << 20),
        };
        Arc::new(BTreeMap::from([(workload.to_owned(), now)]))
    }

    #[tokio::test]
    async fn each_reading_of_the_workload_is_sent_once_with_its_rates_and_no_other() {
        let (publish, readings) = watch::channel(Readings::default());
        let (to_node, incoming) = mpsc::channel(1);
        let (outgoing, mut sent) = mpsc::channel(8);
        let serving = tokio::spawn(serve(readings, opened(Vec::new()), Io { incoming, outgoing }));

        publish.send_replace(reading("wl_other", 10, 0));
        publish.send_replace(reading("wl_a", 10, 0));
        let first = pb::UsageNow::decode_from_slice(&sent.recv().await.unwrap()).unwrap();
        assert_eq!(first.memory_bytes, Some(1 << 20));
        assert_eq!(first.cpu_millicores, None);
        publish.send_replace(reading("wl_a", 20, 2_000_000));
        let second = pb::UsageNow::decode_from_slice(&sent.recv().await.unwrap()).unwrap();
        assert_eq!(second.cpu_millicores, Some(200), "two seconds over ten");
        assert!(sent.try_recv().is_err(), "nothing else, and not another workload's");

        drop(to_node);
        assert_eq!(serving.await.unwrap(), Ok(()), "the client is done");
        let (_publish, readings) = watch::channel(Readings::default());
        let (_to_node, incoming) = mpsc::channel(1);
        let (outgoing, _sent) = mpsc::channel(1);
        let refused = serve(readings, opened(vec![1]), Io { incoming, outgoing }).await;
        assert_eq!(refused.unwrap_err().code, "invalid");
    }
}
