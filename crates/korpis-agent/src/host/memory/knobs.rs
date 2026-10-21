// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a test reads of an in-memory host, and sets on it.

use super::InMemoryHost;
use korpis_proto::korpis::helper::v1 as helper;
use std::collections::{BTreeMap, BTreeSet};

impl InMemoryHost {
    /// The volumes, by identifier, with their paths.
    #[must_use]
    pub fn volumes(&self) -> BTreeMap<String, String> {
        self.with(|k| k.volumes.clone()).unwrap_or_default()
    }

    /// The content fetched.
    #[must_use]
    pub fn content(&self) -> BTreeSet<String> {
        self.with(|k| k.content.clone()).unwrap_or_default()
    }

    /// Makes the artifact `reference` available with `files`, by path and digest.
    pub fn offer_files(&self, reference: &str, files: BTreeMap<String, String>) {
        let _ = self.with(|k| k.files.insert(reference.to_owned(), files));
    }

    /// What job `job` was started with, if it was.
    #[must_use]
    pub fn started(&self, job: &str) -> Option<helper::StartJobRequest> {
        self.with(|k| k.started.get(job).cloned()).ok().flatten()
    }

    /// The socket the ingress proxy was last started to connect to, while it runs, and how many
    /// times it was started.
    #[must_use]
    pub fn ingress(&self) -> (Option<std::path::PathBuf>, u32) {
        self.with(|k| (k.ingress.clone(), k.ingress_starts)).unwrap_or_default()
    }

    /// Every job started, by name.
    #[must_use]
    pub fn started_jobs(&self) -> Vec<String> {
        self.with(|k| k.started.keys().cloned().collect()).unwrap_or_default()
    }

    /// Holds `bytes` as content `digest`.
    pub fn offer_content(&self, digest: &str, bytes: &[u8]) {
        let _ = self.with(|k| k.blobs.insert(digest.to_owned(), bytes.to_vec()));
    }

    /// The snapshots, by volume and snapshot.
    #[must_use]
    pub fn snapshots(&self) -> BTreeSet<(String, String)> {
        self.with(|k| k.snapshots.clone()).unwrap_or_default()
    }

    /// The id ranges recorded, by workload: base and count.
    #[must_use]
    pub fn ids(&self) -> BTreeMap<String, (u32, u32)> {
        self.with(|k| k.ids.clone()).unwrap_or_default()
    }

    /// Volume `volume`'s filesystem accounts `bytes` from now on.
    pub fn set_usage(&self, volume: &str, bytes: u64) {
        let _ = self.with(|k| k.usage.insert(volume.to_owned(), bytes));
    }

    /// The host's table counts `counters` from now on, as `ReadCounters` lists them.
    pub fn set_counters(&self, counters: Vec<helper::Counter>) {
        let _ = self.with(|k| k.counters = counters);
    }
}
