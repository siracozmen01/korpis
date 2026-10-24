// SPDX-License-Identifier: Apache-2.0

//! What the suite is given: the driver, by its socket, and the node it would run on, by the paths
//! the driver was started with (§8 of docs/04-runtimes.md). The suite writes into the content
//! store and the volume root as the agent and the helper would, and the driver only reads them.

use std::path::PathBuf;

/// The driver under test and the node it serves.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// The driver's socket, on which it serves `korpis.driver.v1`.
    pub socket: PathBuf,
    /// The node's content store, as the driver was started with it: blobs by digest under
    /// `blobs/sha256/`.
    pub content: PathBuf,
    /// The directory volumes are kept under, as the driver was started with it.
    pub volume_root: PathBuf,
    /// The probe program (`korpis-conformance-probe`), built static for this machine.
    pub probe: PathBuf,
    /// The first id of the ranges the suite's objects run as, a range of [`Fixture::uid_count`]
    /// each, above any a user of the machine holds.
    pub uid_base: u32,
    /// How many ids each object is given.
    pub uid_count: u32,
    /// How many objects at most the suite makes, which bounds the ids it takes.
    pub objects: u32,
}

impl Fixture {
    /// The id range of the suite's `n`th object, if the fixture has one for it.
    #[must_use]
    pub fn ids(&self, n: u32) -> Option<(u32, u32)> {
        if n >= self.objects {
            return None;
        }
        let base = self.uid_base.checked_add(n.checked_mul(self.uid_count)?)?;
        base.checked_add(self.uid_count)?;
        Some((base, self.uid_count))
    }
}
