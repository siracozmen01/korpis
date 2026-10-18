// SPDX-License-Identifier: AGPL-3.0-or-later

//! The console streams the node writes, and how far into each one's changes log the agent has
//! journaled (§4 of docs/14-streams.md). Kept in a small text file replaced atomically, one line
//! per workload: its name, the byte of its changes log to read from next, and its directory.
//! A change is journaled before the position past it is kept here, so a crash between the two
//! journals it again, which the control plane's index takes as the same segment, rather than
//! losing it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::sync::watch;

/// The name a stream is watched, located, and offloaded under: its workload's, and for a run's
/// console its workload's, `/`, and its run's (§4 of docs/14-streams.md). A wire identifier has
/// neither `/` nor a space, so a name is one word of the file the streams are kept in.
#[must_use]
pub fn stream_name(workload: &str, run: &str) -> String {
    if run.is_empty() { workload.to_owned() } else { format!("{workload}/{run}") }
}

/// The workload and the run, empty for a workload's own, that a stream name names.
#[must_use]
pub fn name_parts(name: &str) -> (&str, &str) {
    name.split_once('/').unwrap_or((name, ""))
}

/// Where each watched stream is, by its name, as the data path reads it.
pub type Located = watch::Receiver<BTreeMap<String, PathBuf>>;

/// One stream watched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    /// Its directory, where the helper put its volume.
    pub dir: PathBuf,
    /// The byte of its changes log to read from next.
    pub next: u64,
}

/// The streams watched.
#[derive(Debug)]
pub struct Streams {
    path: PathBuf,
    watched: BTreeMap<String, Watched>,
    located: watch::Sender<BTreeMap<String, PathBuf>>,
}

fn invalid(line: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("streams: {line:?}"))
}

fn parse(text: &str) -> std::io::Result<BTreeMap<String, Watched>> {
    let mut watched = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut words = line.splitn(3, ' ');
        let (Some(workload), Some(next), Some(dir)) = (words.next(), words.next(), words.next())
        else {
            return Err(invalid(line));
        };
        let next = next.parse().map_err(|_| invalid(line))?;
        watched.insert(workload.to_owned(), Watched { dir: dir.into(), next });
    }
    Ok(watched)
}

impl Streams {
    /// The streams kept at `path`; none kept is none watched.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let watched = match std::fs::read_to_string(path) {
            Ok(text) => parse(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        let (located, _) = watch::channel(BTreeMap::new());
        let streams = Self { path: path.to_owned(), watched, located };
        streams.publish();
        Ok(streams)
    }

    /// Where each watched stream is, kept current as streams are watched and no longer are.
    #[must_use]
    pub fn located(&self) -> Located {
        self.located.subscribe()
    }

    fn publish(&self) {
        let dirs = self.watched.iter().map(|(w, s)| (w.clone(), s.dir.clone())).collect();
        self.located.send_replace(dirs);
    }

    /// Every stream watched.
    #[must_use]
    pub fn watched(&self) -> Vec<(String, Watched)> {
        self.watched.iter().map(|(w, s)| (w.clone(), s.clone())).collect()
    }

    /// Watches `workload`'s stream in `dir`, from where it was if it was already watched there.
    pub fn watch(&mut self, workload: &str, dir: &Path) -> std::io::Result<()> {
        if self.watched.get(workload).is_some_and(|s| s.dir == dir) {
            return Ok(());
        }
        self.watched.insert(workload.to_owned(), Watched { dir: dir.to_owned(), next: 0 });
        self.publish();
        self.save()
    }

    /// Keeps that `workload`'s changes log is journaled up to `next`.
    pub fn read_to(&mut self, workload: &str, next: u64) -> std::io::Result<()> {
        match self.watched.get_mut(workload) {
            Some(s) if s.next != next => s.next = next,
            _ => return Ok(()),
        }
        self.save()
    }

    /// Stops watching `workload`'s stream; what it wrote stays where it is.
    pub fn unwatch(&mut self, workload: &str) -> std::io::Result<()> {
        if self.watched.remove(workload).is_none() {
            return Ok(());
        }
        self.publish();
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        let lines: Vec<String> = self
            .watched
            .iter()
            .map(|(w, s)| format!("{w} {} {}\n", s.next, s.dir.display()))
            .collect();
        let beside = self.path.with_extension("new");
        std::fs::write(&beside, lines.concat())?;
        std::fs::File::open(&beside)?.sync_all()?;
        std::fs::rename(&beside, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_watched_and_how_far_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("streams");
        let mut s = Streams::open(&path).unwrap();
        s.watch("wl_1", Path::new("/var/lib/korpis/volumes/wl 1.stream_console")).unwrap();
        s.watch("wl_2", Path::new("/v/wl_2.stream_console")).unwrap();
        s.read_to("wl_1", 120).unwrap();
        s.watch("wl_1", Path::new("/var/lib/korpis/volumes/wl 1.stream_console")).unwrap();
        s.unwatch("wl_2").unwrap();
        let again = Streams::open(&path).unwrap();
        let expected =
            Watched { dir: "/var/lib/korpis/volumes/wl 1.stream_console".into(), next: 120 };
        assert_eq!(again.watched(), [("wl_1".to_owned(), expected)], "a path with a space, kept");
        std::fs::write(&path, "wl_1 many /x\n").unwrap();
        assert!(Streams::open(&path).is_err());
    }

    #[test]
    fn a_run_s_stream_is_named_beneath_its_workload_and_a_workload_s_by_it_alone() {
        assert_eq!(stream_name("wl_1", ""), "wl_1");
        assert_eq!(stream_name("wl_1", "run_2"), "wl_1/run_2");
        assert_eq!(name_parts("wl_1/run_2"), ("wl_1", "run_2"));
        assert_eq!(name_parts("wl_1"), ("wl_1", ""));
    }
}
