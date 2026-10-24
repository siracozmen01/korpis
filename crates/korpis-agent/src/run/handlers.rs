// SPDX-License-Identifier: AGPL-3.0-or-later

//! The data path's handlers `korpis-agent run` serves: the console's, a workload's usage now, its
//! file sessions, and reading its block devices' backups.

use crate::data::backup::{self, BackupSessions, Reading};
use crate::data::console::{self, ConsoleRead};
use crate::data::console_write::{self, ConsoleWrite};
use crate::data::files::{self, FileSessions};
use crate::data::handler::Handlers;
use crate::data::usage::{self, UsageRead};
use crate::host::Host;
use crate::journal::JournalHandle;
use crate::meter::now::Readings;
use crate::reconcile::streams::Streams;
use crate::runtime::Runtime;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::watch::{Receiver, Sender};

/// The data path's handlers, with the console's own: reading the streams the node holds, and
/// writing to the runtime object each workload runs as; and what a workload uses now, from the
/// meter's `readings`.
pub(super) fn consoles<R: Runtime + Clone>(
    handlers: Handlers,
    (streams, readings): (&Streams, Receiver<Readings>),
    (runtime, runtimes): (&R, &Sender<BTreeMap<String, String>>),
    journal: &JournalHandle,
) -> Handlers {
    let write = ConsoleWrite::new(runtime.clone(), runtimes.subscribe(), journal.clone());
    handlers
        .with(console::ACTION, Arc::new(ConsoleRead::new(streams.located(), journal.clone())))
        .with(console_write::ACTION, Arc::new(write))
        .with(usage::ACTION, Arc::new(UsageRead::new(readings)))
}

/// The data path's handlers, with the file session's: through `host`'s helper, or through
/// `runtime` on the object `runtimes` names where its driver serves files inside it; and reading
/// a block device's backup, its job's access asked of `jobs` and the job held in `reading`.
pub(super) fn file_sessions<R: Runtime + Clone, H: Host + Clone>(
    handlers: Handlers,
    (host, runtime): (&H, &R),
    (runtimes, held): (&Sender<BTreeMap<String, String>>, &Receiver<crate::held::HeldSet>),
    (journal, jobs, reading): (&JournalHandle, &Arc<crate::jobs::JobAccesses>, &Reading),
) -> Handlers {
    let both = (host.clone(), runtime.clone());
    let files =
        FileSessions::new(both.clone(), runtimes.subscribe(), held.clone(), journal.clone());
    let files = Arc::new(files);
    let access = (Arc::clone(jobs), reading.clone());
    let backups = Arc::new(BackupSessions::new(both, held.clone(), journal.clone(), access));
    handlers
        .with(files::READ, files.clone())
        .with(files::WRITE, files)
        .with(backup::BROWSE, backups.clone())
        .with(backup::READ, backups)
}
