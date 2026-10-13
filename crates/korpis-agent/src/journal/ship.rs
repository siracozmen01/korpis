// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sending the journal on a session (§4.10 of docs/02-architecture.md). The control plane opens
//! every session with its cursor; the shipper sends everything after it, and from then on what
//! is appended, a bounded number of records ahead of what has been acknowledged, so a backlog
//! is paced by the control plane's ingestion rather than pushed down the stream at once. If the
//! acknowledgements stop while records are outstanding, it sends again from the cursor: the
//! control plane drops what it already has.

use super::JournalHandle;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use std::time::{Duration, Instant};

/// About how many bytes of records one message carries.
const BATCH: u64 = 1 << 20;
/// How many numbers may be sent and not yet acknowledged.
const IN_FLIGHT: u64 = 4096;
/// How many messages one call sends at most, so a backlog never starves the heartbeat.
const BURST: usize = 8;
/// How long acknowledgements may stop while records are outstanding before they are sent again.
const RESEND_AFTER: Duration = Duration::from_secs(30);

/// One session's shipper.
#[derive(Debug)]
pub struct Shipper {
    journal: JournalHandle,
    started: bool,
    sent: u64,
    acked: u64,
    progress: Instant,
}

fn message(records: Vec<agent::JournalRecord>) -> agent::AgentMessage {
    let journal = agent::JournalRecords { records, ..Default::default() };
    agent::AgentMessage { message: Some(Up::Journal(Box::new(journal))), ..Default::default() }
}

impl Shipper {
    /// A shipper for a session just opened: it sends nothing before the control plane's cursor.
    #[must_use]
    pub fn new(journal: JournalHandle) -> Self {
        Self { journal, started: false, sent: 0, acked: 0, progress: Instant::now() }
    }

    /// The control plane's cursor is `seq`: the journal may discard through it after retention,
    /// and what follows it is sent.
    pub async fn acknowledged(&mut self, seq: u64) -> std::io::Result<Vec<agent::AgentMessage>> {
        self.journal.acknowledge(seq).await?;
        if !self.started || seq > self.acked {
            self.acked = seq;
            self.progress = Instant::now();
        }
        if !self.started {
            self.started = true;
            self.sent = seq;
        }
        self.sent = self.sent.max(seq);
        self.pump().await
    }

    /// What is ready to send: records after those sent, within the window.
    pub async fn pump(&mut self) -> std::io::Result<Vec<agent::AgentMessage>> {
        let mut out = Vec::new();
        while self.started && self.sent.saturating_sub(self.acked) < IN_FLIGHT && out.len() < BURST
        {
            let records = self.journal.read_after(self.sent, BATCH).await?;
            let Some(last) = records.last().map(|r| r.seq) else { break };
            self.sent = last;
            out.push(message(records));
        }
        Ok(out)
    }

    /// If acknowledgements stopped with records outstanding, everything after the cursor again.
    pub async fn stalled(&mut self) -> std::io::Result<Vec<agent::AgentMessage>> {
        if !self.started || self.sent <= self.acked || self.progress.elapsed() < RESEND_AFTER {
            return Ok(Vec::new());
        }
        tracing::warn!(cursor = self.acked, sent = self.sent, "journal unacknowledged; resending");
        self.sent = self.acked;
        self.progress = Instant::now();
        self.pump().await
    }
}
