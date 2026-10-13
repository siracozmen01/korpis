// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent, assembled: the control stream, the journal and its maintenance, the lease keeper,
//! and the reconciler, over one runtime driver and the node's helper (§6 of
//! docs/02-architecture.md). It starts whether or not the control plane is reachable, because a
//! node keeps doing its job from the set it holds (§4.6), and a clean stop writes what the
//! journal still holds in memory before the process exits.

use crate::host::Host;
use crate::journal::{Bounds, JournalHandle};
use crate::lease::{Keeper, Leases};
use crate::reconcile::progress::Progress;
use crate::reconcile::{Reconciler, Wiring};
use crate::runtime::{Runtime, RuntimeError};
use crate::session::{Event, Facts, Session};
use crate::state::{StateDir, StateError};
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Why the agent could not start.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The node has not joined, or its state does not read.
    #[error(transparent)]
    State(#[from] StateError),
    /// The journal or the reconciler's progress could not be opened.
    #[error("{what}: {source}")]
    Io {
        /// Which.
        what: &'static str,
        /// What failed.
        source: std::io::Error,
    },
    /// The driver could not be asked what it is.
    #[error("the driver: {0}")]
    Driver(#[from] RuntimeError),
}

/// What the node keeps back from placement for its own system, agent, and runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reserve {
    /// Millicores.
    pub cpu_millis: u64,
    /// Bytes.
    pub memory_bytes: u64,
}

impl Default for Reserve {
    fn default() -> Self {
        Self { cpu_millis: 500, memory_bytes: 1 << 30 }
    }
}

/// How the agent runs.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// How often the reconciler looks without being told to.
    pub period: Duration,
    /// How often the journal is maintained.
    pub maintain_every: Duration,
    /// The journal's bound.
    pub journal: Bounds,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            period: Duration::from_secs(2),
            maintain_every: Duration::from_secs(5),
            journal: Bounds::default(),
        }
    }
}

/// Total memory, from `/proc/meminfo`; zero where it cannot be read.
fn memory_total() -> u64 {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    text.lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or(0, |kib| kib.saturating_mul(1024))
}

/// This machine's capacity, and what placement may reserve of it: capacity less `reserve`,
/// each part of which is capped at a quarter of capacity so a small machine is not reserved away
/// (§2.1 of docs/05-scheduling.md).
#[must_use]
pub fn machine(reserve: Reserve) -> (pb::Resources, pb::Resources) {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let cpu = u64::try_from(cores).unwrap_or(1).saturating_mul(1000);
    let memory = memory_total();
    let capacity = pb::Resources { cpu_millis: cpu, memory_bytes: memory, ..Default::default() };
    let allocatable = pb::Resources {
        cpu_millis: cpu.saturating_sub(reserve.cpu_millis.min(cpu / 4)),
        memory_bytes: memory.saturating_sub(reserve.memory_bytes.min(memory / 4)),
        ..Default::default()
    };
    (capacity, allocatable)
}

/// The facts a node reports: its driver, as the driver declares itself, and this machine.
pub async fn facts_of<R: Runtime>(
    runtime: &R,
    reserve: Reserve,
) -> Result<Arc<dyn Fn() -> Facts + Send + Sync>, RuntimeError> {
    let driver = runtime.describe().await?;
    let (capacity, allocatable) = machine(reserve);
    let facts = Facts { drivers: vec![driver], storage_classes: Vec::new(), capacity, allocatable };
    Ok(Arc::new(move || facts.clone()))
}

/// The running agent.
#[derive(Debug)]
pub struct Agent {
    tasks: Vec<JoinHandle<()>>,
    journal: JournalHandle,
    events: broadcast::Sender<Event>,
}

impl Agent {
    /// Starts the agent of the node that joined into `state`, driving `runtime` and `host`;
    /// called inside a Tokio runtime, whose tasks it spawns.
    pub fn start<R: Runtime, H: Host>(
        state: &StateDir,
        runtime: R,
        host: H,
        facts: Arc<dyn Fn() -> Facts + Send + Sync>,
        options: Options,
    ) -> Result<Self, RunError> {
        let identity = state.load()?;
        let io = |what| move |source| RunError::Io { what, source };
        let journal = JournalHandle::open(&state.root().join("journal"), options.journal)
            .map_err(io("the journal"))?;
        let progress =
            Progress::open(&state.root().join("progress")).map_err(io("the progress"))?;
        let node = identity.node.clone();
        let (session, link) = Session::new(identity, state.clone(), facts, journal.clone());
        let leases = Leases::new(journal.clone());
        let keeper =
            Keeper::new(leases.clone(), link.held.clone(), link.inbound, link.outbound.clone());
        let wiring = Wiring {
            node,
            held: link.held.clone(),
            leases,
            journal: journal.clone(),
            outbound: link.outbound.clone(),
            events: link.events.subscribe(),
            period: options.period,
        };
        let reconciler = Reconciler::new(runtime, host, wiring, progress);
        let tasks = vec![
            tokio::spawn(session.run()),
            tokio::spawn(keeper.run()),
            journal.maintain_every(options.maintain_every),
            tokio::spawn(reconciler.run()),
        ];
        Ok(Self { tasks, journal, events: link.events })
    }

    /// What happens on the control stream, from now on.
    #[must_use]
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Stops the agent, leaving every workload as it is, and writes what the journal still
    /// counts in memory.
    pub async fn stop(self) {
        for t in &self.tasks {
            t.abort();
        }
        if let Err(e) = self.journal.close().await {
            tracing::error!(error = %e, "the journal could not be closed cleanly");
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserve_never_takes_more_than_a_quarter_of_the_machine() {
        let (capacity, allocatable) = machine(Reserve::default());
        assert!(capacity.cpu_millis >= 1000);
        assert!(allocatable.cpu_millis <= capacity.cpu_millis);
        let greedy = Reserve { cpu_millis: u64::MAX, memory_bytes: u64::MAX };
        let (capacity, allocatable) = machine(greedy);
        assert!(allocatable.cpu_millis >= capacity.cpu_millis / 4 * 3);
        assert!(allocatable.memory_bytes >= capacity.memory_bytes / 4 * 3);
    }
}
