// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tree verbs, kept in memory: a send writes a line naming what it sends into its socket and
//! is done once the far side closes; a receive reads its socket to the end on a thread, and is
//! done when anything came, keeping what did.

use super::{InMemoryHost, Kept};
use crate::host::{HostError, Trees};
use korpis_proto::korpis::helper::v1 as helper;
use std::future::{Future, ready};
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

type Key = (String, String, bool);

fn io(e: &std::io::Error) -> HostError {
    HostError(format!("a socket: {e}"))
}

/// Reads `ours` to its end, then records the pass `key` as done with what it read, or failed
/// when nothing came.
fn ended(kept: &Arc<Mutex<Kept>>, key: Key, mut ours: UnixStream) {
    let mut got = Vec::new();
    let read = ours.read_to_end(&mut got);
    // A sender is done once the far side closed; a receiver once anything came.
    let state = match read {
        Ok(n) if n > 0 || key.2 => helper::TreeState::TREE_STATE_DONE,
        _ => helper::TreeState::TREE_STATE_FAILED,
    };
    if let Ok(mut k) = kept.lock() {
        k.trees.insert(key, (state, got));
    }
}

impl InMemoryHost {
    /// What the receiving side of the pass of `volume` named `pass` read, once it ended.
    #[must_use]
    pub fn tree_received(&self, volume: &str, pass: &str) -> Option<Vec<u8>> {
        let key = (volume.to_owned(), pass.to_owned(), false);
        let done = helper::TreeState::TREE_STATE_DONE;
        self.with(|k| k.trees.get(&key).filter(|(s, _)| *s == done).map(|(_, b)| b.clone()))
            .ok()
            .flatten()
    }

    fn start(&self, key: Key, line: Option<String>) -> Result<OwnedFd, HostError> {
        let (theirs, mut ours) = UnixStream::pair().map_err(|e| io(&e))?;
        if let Some(line) = line {
            ours.write_all(line.as_bytes()).map_err(|e| io(&e))?;
            ours.shutdown(std::net::Shutdown::Write).map_err(|e| io(&e))?;
        }
        self.with(|k| {
            k.trees.insert(key.clone(), (helper::TreeState::TREE_STATE_RUNNING, Vec::new()));
        })?;
        let kept = Arc::clone(&self.kept);
        std::thread::spawn(move || ended(&kept, key, ours));
        Ok(OwnedFd::from(theirs))
    }
}

impl Trees for InMemoryHost {
    fn send_tree(
        &self,
        r: &helper::SendTreeRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send {
        let line = format!("tree {} pass {}\n", r.volume, r.pass);
        ready(self.start((r.volume.clone(), r.pass.clone(), true), Some(line)))
    }

    fn receive_tree(
        &self,
        r: &helper::ReceiveTreeRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send {
        ready(self.start((r.volume.clone(), r.pass.clone(), false), None))
    }

    fn tree_status(
        &self,
        r: &helper::TreeStatusRequest,
    ) -> impl Future<Output = Result<helper::TreeStatusResponse, HostError>> + Send {
        let key = (r.volume.clone(), r.pass.clone(), r.sending);
        let state = self
            .with(|k| k.trees.get(&key).map_or(helper::TreeState::TREE_STATE_UNKNOWN, |(s, _)| *s));
        ready(state.map(|s| {
            let sent_bytes =
                if !r.sending && s == helper::TreeState::TREE_STATE_DONE { 4096 } else { 0 };
            helper::TreeStatusResponse { state: s.into(), sent_bytes, ..Default::default() }
        }))
    }
}
