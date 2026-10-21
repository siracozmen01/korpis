// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's meter (§2 of docs/15-observability.md, "Measuring it, as designed"). It knows every
//! runtime object the node runs and the workload each is (the [`metered`] runtime tells it), reads
//! each object's counters every ten seconds and the workload's interface beside them, keeps its
//! accounts in a file of its own replaced at every reading, and at the end of each metering
//! interval journals one sample for every workload the node holds an assignment for.

pub mod book;
pub mod metered;
mod sample;

use crate::held::HeldSet;
use crate::host::Host;
use crate::journal::JournalHandle;
use crate::runtime::Runtime;
use book::Book;
use buffa::Message as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;
use metered::Metered;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::sync::watch;

pub use sample::{Sampler, Sources};

/// How often the meter reads every source.
pub const READ_EVERY: Duration = Duration::from_secs(10);
/// The metering interval where the installation declares none: an hour (§7 of
/// docs/15-observability.md, question 1).
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(3600);

/// How the meter runs.
#[derive(Debug, Clone)]
pub struct Metering {
    /// The metering interval.
    pub interval: Duration,
    /// Where the kernel lists network interfaces.
    pub net: PathBuf,
}

impl Default for Metering {
    fn default() -> Self {
        Self { interval: DEFAULT_INTERVAL, net: PathBuf::from("/sys/class/net") }
    }
}

/// When the machine booted, by `/proc/stat`'s `btime`; none where it cannot be read.
#[must_use]
pub fn boot_time() -> Option<SystemTime> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let seconds: u64 = stat.lines().find_map(|l| l.strip_prefix("btime "))?.trim().parse().ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
}

/// A runtime metered, and the sampler that reads it.
pub type Started<R, H> = (Metered<R>, Sampler<Metered<R>, H>);

/// The meter of the node whose state is in `root`, kept in its `meter` file: `runtime` metered,
/// and the sampler that reads it, `host`'s volumes, and the interfaces of what `held` names,
/// journaling into `journal`.
pub fn start<R: Runtime + Clone, H: Host>(
    root: &Path,
    runtime: R,
    (host, held, journal): (H, watch::Receiver<HeldSet>, JournalHandle),
    options: Metering,
) -> std::io::Result<Started<R, H>> {
    let meter = Arc::new(Meter::open(&root.join("meter"), boot_time())?);
    let metered = Metered::new(runtime, meter.clone());
    let sources = Sources { runtime: metered.clone(), host, held, net: options.net };
    Ok((metered.clone(), Sampler::new(meter, sources, journal, options.interval)))
}

/// What a driver's counters come to as the meter's.
#[must_use]
pub fn reading(c: &driver::ObjectCounters) -> agent::MeterCounters {
    agent::MeterCounters {
        cpu_usec: c.cpu_usec,
        cpu_throttled_usec: c.cpu_throttled_usec,
        io_read_bytes: c.io_read_bytes,
        io_write_bytes: c.io_write_bytes,
        ..Default::default()
    }
}

/// The meter: its accounts, and the objects it reads.
#[derive(Debug)]
pub struct Meter {
    book: Mutex<Book>,
    /// Each runtime object the node runs, by identifier: the workload it is.
    live: Mutex<BTreeMap<String, String>>,
    path: PathBuf,
}

impl Meter {
    /// The meter whose accounts are kept at `path`, as it last kept them. A machine that booted
    /// at `boot`, after the meter last read its sources, lost what they used since: each
    /// source's last reading joins its base, and the span is a loss.
    pub fn open(path: &Path, boot: Option<SystemTime>) -> std::io::Result<Self> {
        let mut accounts = match std::fs::read(path) {
            Ok(bytes) => {
                Book::from_kept(agent::MeterBook::decode_from_slice(&bytes).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("meter: {e}"))
                })?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Book::default(),
            Err(e) => return Err(e),
        };
        if let (Some(boot), Some(read)) = (boot, accounts.read_at())
            && read < boot
        {
            accounts.rebooted(boot);
        }
        Ok(Self { book: Mutex::new(accounts), live: Mutex::default(), path: path.to_owned() })
    }

    pub(crate) fn with_book<T>(&self, f: impl FnOnce(&mut Book) -> T) -> T {
        let mut book = self.book.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut book)
    }

    fn with_live<T>(&self, f: impl FnOnce(&mut BTreeMap<String, String>) -> T) -> T {
        let mut live = self.live.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut live)
    }

    /// `object` is one of `workload`'s.
    pub fn track(&self, object: &str, workload: &str) {
        self.with_live(|l| l.insert(object.to_owned(), workload.to_owned()));
    }

    /// The workload `object` is, if the meter knows it.
    #[must_use]
    pub fn workload_of(&self, object: &str) -> Option<String> {
        self.with_live(|l| l.get(object).cloned())
    }

    /// Every object it reads, with its workload.
    #[must_use]
    pub fn live(&self) -> Vec<(String, String)> {
        self.with_live(|l| l.iter().map(|(o, w)| (o.clone(), w.clone())).collect())
    }

    /// `object` ended, its last counters `last`, or none where they could not be read.
    pub fn ended(&self, object: &str, last: Option<&driver::ObjectCounters>) {
        let Some(workload) = self.with_live(|l| l.remove(object)) else { return };
        let now = SystemTime::now();
        self.with_book(|b| b.ended(&workload, object, last.map(reading), now));
        self.keep();
    }

    /// Writes the accounts to the meter's file; a failure is logged, and the next reading
    /// writes them again.
    pub fn keep(&self) {
        let bytes = self.with_book(|b| b.to_kept().encode_to_vec());
        if let Err(e) = crate::durable::replace(&self.path, &bytes) {
            tracing::warn!(error = %e, "the meter's accounts could not be kept");
        }
    }
}

#[cfg(test)]
mod tests;
