// SPDX-License-Identifier: AGPL-3.0-or-later

//! The id ranges the node's workloads hold, as their assignments name them (§4.5 of
//! docs/04-runtimes.md), recorded on disk. The helper refuses any range that is not exactly one
//! workload slot, so no request makes it treat a host's own ids, root's above all, as a
//! workload's. The control plane gives a slot again only once its workload is retired, so a
//! range recorded for another workload is a retired one's, and the newer record replaces it.

use crate::error::{HelperError, HelperResult, identifier};
use korpis_core::workload::assignment::IdRange;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The recorded ranges, by base.
#[derive(Debug)]
pub struct Ledger {
    path: PathBuf,
    ranges: BTreeMap<u32, String>,
}

fn parse(text: &str) -> BTreeMap<u32, String> {
    text.lines()
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(base, workload)| Some((base.parse().ok()?, workload.to_owned())))
        .collect()
}

impl Ledger {
    /// The ledger kept in `dir`, created if missing.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("id-ranges");
        let ranges = match std::fs::read_to_string(&path) {
            Ok(text) => parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, ranges })
    }

    /// The workload holding the range at `base`, if one does.
    #[must_use]
    pub fn holder(&self, base: u32) -> Option<&str> {
        self.ranges.get(&base).map(String::as_str)
    }

    /// Records that `workload` holds `range`.
    pub fn ensure(&mut self, workload: &str, range: IdRange) -> HelperResult<()> {
        identifier("workload", workload)?;
        if range.slot().is_none() {
            tracing::warn!(%workload, base = range.base, count = range.count, "not a workload range");
            return Err(HelperError::Refused("not a workload's id range".into()));
        }
        if self.holder(range.base) == Some(workload) {
            return Ok(());
        }
        if let Some(before) = self.ranges.insert(range.base, workload.to_owned()) {
            tracing::info!(base = range.base, %before, %workload, "a retired range is given again");
        }
        self.ranges.retain(|base, w| *base == range.base || w != workload);
        self.save()
    }

    /// Writes the ledger whole, durably, replacing the old one only once the new one is on disk.
    fn save(&self) -> HelperResult<()> {
        let tmp = self.path.with_extension("tmp");
        let mut f = std::fs::File::create(&tmp)?;
        for (base, workload) in &self.ranges {
            writeln!(f, "{base} {workload}")?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        if let Some(dir) = self.path.parent() {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workload_holds_one_slot_and_host_ids_are_never_a_workloads() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Ledger::open(dir.path()).unwrap();
        let (a, b) = (IdRange::of_slot(0).unwrap(), IdRange::of_slot(1).unwrap());
        l.ensure("wl_a", a).unwrap();
        l.ensure("wl_a", b).unwrap();
        assert_eq!((l.holder(a.base), l.holder(b.base)), (None, Some("wl_a")), "one slot each");
        l.ensure("wl_b", b).unwrap();
        assert_eq!(Ledger::open(dir.path()).unwrap().holder(b.base), Some("wl_b"), "on disk");
        let root = IdRange { base: 0, count: 65536 };
        assert!(matches!(l.ensure("wl_c", root), Err(HelperError::Refused(_))));
        assert!(matches!(l.ensure("../x", a), Err(HelperError::Refused(_))));
    }
}
