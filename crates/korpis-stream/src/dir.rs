// SPDX-License-Identifier: AGPL-3.0-or-later

//! A stream's directory: segments named by their first offset, `<offset>.open` while written and
//! `<offset>.kseg` once sealed. Sealing writes the footer, then renames, so a crash between the
//! two leaves an `.open` file that is in fact sealed, which opening the directory finishes. Old
//! sealed segments are pruned to the stream's retention, oldest first; the one being written
//! never is.

use crate::segment::{self, Scan, Sealed};
use std::io;
use std::path::{Path, PathBuf};

/// One segment on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDisk {
    /// The offset of its first record.
    pub first_offset: u64,
    /// Where it is.
    pub path: PathBuf,
    /// Whether it is sealed.
    pub sealed: bool,
}

/// A stream's directory.
#[derive(Debug, Clone)]
pub struct StreamDir {
    root: PathBuf,
}

impl StreamDir {
    /// The stream kept under `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where it is.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where a segment starting at `first` is written.
    #[must_use]
    pub fn open_path(&self, first: u64) -> PathBuf {
        self.root.join(format!("{first:020}.open"))
    }

    /// Where it is once sealed.
    #[must_use]
    pub fn sealed_path(&self, first: u64) -> PathBuf {
        self.root.join(format!("{first:020}.kseg"))
    }

    /// Every segment, by first offset.
    pub fn segments(&self) -> io::Result<Vec<OnDisk>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let path = entry?.path();
            let (Some(stem), Some(ext)) = (path.file_stem(), path.extension()) else { continue };
            let Some(first_offset) = stem.to_str().and_then(|s| s.parse().ok()) else { continue };
            let sealed = match ext.to_str() {
                Some("kseg") => true,
                Some("open") => false,
                _ => continue,
            };
            out.push(OnDisk { first_offset, path, sealed });
        }
        out.sort_by_key(|s| s.first_offset);
        Ok(out)
    }

    /// Renames the sealed segment starting at `first` to its sealed name.
    pub fn mark_sealed(&self, first: u64) -> io::Result<()> {
        std::fs::rename(self.open_path(first), self.sealed_path(first))?;
        sync_dir(&self.root)
    }

    /// Finishes a seal a crash interrupted, for every `.open` file that is already sealed; the
    /// segment still being written, if any, and how it scans.
    pub fn settle(&self) -> io::Result<Option<(OnDisk, Scan)>> {
        let mut writing = None;
        for s in self.segments()?.into_iter().filter(|s| !s.sealed) {
            let scan = segment::scan(&s.path)?;
            if scan.sealed.is_some() {
                self.mark_sealed(s.first_offset)?;
            } else {
                writing = Some((s, scan));
            }
        }
        Ok(writing)
    }

    /// Removes sealed segments whose last record is older than `cutoff` (nanoseconds since the
    /// Unix epoch); what they were.
    pub fn prune_before(&self, cutoff: i64) -> io::Result<Vec<Sealed>> {
        let mut removed = Vec::new();
        for s in self.segments()?.into_iter().filter(|s| s.sealed) {
            let Some(info) = segment::sealed_info(&s.path)? else { continue };
            if info.times.1 >= cutoff {
                break;
            }
            std::fs::remove_file(&s.path)?;
            removed.push(info);
        }
        Ok(removed)
    }

    /// Removes sealed segments, oldest first, while the sealed ones hold more than `max_bytes`;
    /// what they were.
    pub fn prune_to(&self, max_bytes: u64) -> io::Result<Vec<Sealed>> {
        let sealed = self
            .segments()?
            .into_iter()
            .filter(|s| s.sealed)
            .map(|s| Ok(segment::sealed_info(&s.path)?.map(|i| (s.path, i))))
            .filter_map(Result::transpose)
            .collect::<io::Result<Vec<_>>>()?;
        let mut total: u64 = sealed.iter().map(|(_, i)| i.bytes).sum();
        let mut removed = Vec::new();
        for (path, info) in sealed {
            if total <= max_bytes {
                break;
            }
            std::fs::remove_file(&path)?;
            total -= info.bytes;
            removed.push(info);
        }
        Ok(removed)
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}
