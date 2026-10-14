// SPDX-License-Identifier: AGPL-3.0-or-later

//! How far into each stream's changes log the offloader has offloaded: a small text file
//! replaced atomically, one line per stream, its workload, the byte of its changes log to read
//! from next, and its directory. A segment is offloaded and journaled before the position past it
//! is kept, so a crash between the two offloads it again, which writes the same object and is the
//! same segment to the index, rather than losing it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// One stream's place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    /// Its directory.
    pub dir: PathBuf,
    /// The byte of its changes log to read from next.
    pub next: u64,
}

/// Every stream's place.
#[derive(Debug)]
pub struct Positions {
    path: PathBuf,
    kept: BTreeMap<String, Position>,
}

fn invalid(line: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("offloaded: {line:?}"))
}

impl Positions {
    /// The places kept at `path`; none kept is none.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let mut kept = BTreeMap::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let mut words = line.splitn(3, ' ');
            let (Some(w), Some(next), Some(dir)) = (words.next(), words.next(), words.next())
            else {
                return Err(invalid(line));
            };
            let next = next.parse().map_err(|_| invalid(line))?;
            kept.insert(w.to_owned(), Position { dir: dir.into(), next });
        }
        Ok(Self { path: path.to_owned(), kept })
    }

    /// Every stream's place.
    #[must_use]
    pub fn all(&self) -> Vec<(String, Position)> {
        self.kept.iter().map(|(w, p)| (w.clone(), p.clone())).collect()
    }

    /// Starts following `workload`'s stream in `dir` from its beginning, unless it is followed.
    pub fn follow(&mut self, workload: &str, dir: &Path) -> std::io::Result<()> {
        if self.kept.get(workload).is_some_and(|p| p.dir == dir) {
            return Ok(());
        }
        self.kept.insert(workload.to_owned(), Position { dir: dir.to_owned(), next: 0 });
        self.save()
    }

    /// Keeps that `workload`'s stream is offloaded up to byte `next` of its changes log.
    pub fn reached(&mut self, workload: &str, next: u64) -> std::io::Result<()> {
        match self.kept.get_mut(workload) {
            Some(p) if p.next != next => p.next = next,
            _ => return Ok(()),
        }
        self.save()
    }

    /// Stops following `workload`'s stream.
    pub fn drop_stream(&mut self, workload: &str) -> std::io::Result<()> {
        if self.kept.remove(workload).is_none() {
            return Ok(());
        }
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        let lines = self.kept.iter().fold(String::new(), |mut s, (w, p)| {
            let _ = writeln!(s, "{w} {} {}", p.next, p.dir.display());
            s
        });
        let beside = self.path.with_extension("new");
        std::fs::write(&beside, lines)?;
        std::fs::File::open(&beside)?.sync_all()?;
        std::fs::rename(&beside, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_survive_a_restart_and_a_dropped_stream_is_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offloaded");
        let mut p = Positions::open(&path).unwrap();
        p.follow("w1", Path::new("/streams/w1")).unwrap();
        p.follow("w2", Path::new("/streams/w2")).unwrap();
        p.reached("w1", 240).unwrap();
        p.follow("w1", Path::new("/streams/w1")).unwrap();
        let again = Positions::open(&path).unwrap();
        assert_eq!(
            again.all()[0],
            ("w1".into(), Position { dir: "/streams/w1".into(), next: 240 })
        );
        let mut again = again;
        again.drop_stream("w2").unwrap();
        assert_eq!(Positions::open(&path).unwrap().all().len(), 1);
    }
}
