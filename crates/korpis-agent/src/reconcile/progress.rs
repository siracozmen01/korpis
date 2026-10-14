// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the reconciler has done that a restart must not do again, kept in a small text file
//! replaced atomically: for each workload, the last plan step it executed and how it turned out,
//! the power cycle it last performed (§4.8 of docs/02-architecture.md: each cycle once), and how
//! far its install has come, which is kept per install rather than per plan, so a new plan that
//! runs the same install resumes it at the action that failed (§4 of docs/09-recipes.md).
//! A step's completion is journalled before it is kept here, so a crash between the two runs the
//! step again, which is a no-op that reports success, rather than losing the completion.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The last step of a plan a workload executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The plan.
    pub plan: String,
    /// The step's index.
    pub index: u32,
    /// Whether it failed: a failed plan is not executed further.
    pub failed: bool,
}

/// How far an install has come.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// Which install: a digest of the workload and the actions it runs.
    pub digest: String,
    /// How many of its actions have run.
    pub done: u32,
    /// How many times in a row its worker was lost before it ended.
    pub lost: u32,
}

/// Per workload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Done {
    /// The last step executed.
    pub step: Option<Step>,
    /// The last power cycle performed.
    pub cycle: Option<u64>,
    /// The last install begun.
    pub install: Option<Install>,
}

/// The reconciler's progress.
#[derive(Debug)]
pub struct Progress {
    path: PathBuf,
    done: BTreeMap<String, Done>,
}

fn invalid(line: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("progress: {line:?}"))
}

fn parse(text: &str) -> std::io::Result<BTreeMap<String, Done>> {
    let mut done: BTreeMap<String, Done> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["step", w, plan, index, outcome] => {
                let index = index.parse().map_err(|_| invalid(line))?;
                let failed = match *outcome {
                    "ok" => false,
                    "failed" => true,
                    _ => return Err(invalid(line)),
                };
                let step = Step { plan: (*plan).to_owned(), index, failed };
                done.entry((*w).to_owned()).or_default().step = Some(step);
            }
            ["cycle", w, n] => {
                let n = n.parse().map_err(|_| invalid(line))?;
                done.entry((*w).to_owned()).or_default().cycle = Some(n);
            }
            ["install", w, digest, d, l] => {
                let (done_, lost) =
                    (d.parse().map_err(|_| invalid(line))?, l.parse().map_err(|_| invalid(line))?);
                let install = Install { digest: (*digest).to_owned(), done: done_, lost };
                done.entry((*w).to_owned()).or_default().install = Some(install);
            }
            _ => return Err(invalid(line)),
        }
    }
    Ok(done)
}

fn outcome(failed: bool) -> &'static str {
    if failed { "failed" } else { "ok" }
}

impl Progress {
    /// The progress kept at `path`; none kept is none done.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let done = match std::fs::read_to_string(path) {
            Ok(text) => parse(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path: path.to_owned(), done })
    }

    /// What `workload` has done.
    #[must_use]
    pub fn of(&self, workload: &str) -> Done {
        self.done.get(workload).cloned().unwrap_or_default()
    }

    fn save(&self) -> std::io::Result<()> {
        let mut lines = Vec::new();
        for (w, d) in &self.done {
            if let Some(s) = &d.step {
                lines.push(format!("step {w} {} {} {}", s.plan, s.index, outcome(s.failed)));
            }
            if let Some(c) = d.cycle {
                lines.push(format!("cycle {w} {c}"));
            }
            if let Some(i) = &d.install {
                lines.push(format!("install {w} {} {} {}", i.digest, i.done, i.lost));
            }
        }
        crate::durable::replace(&self.path, (lines.join("\n") + "\n").as_bytes())
    }

    /// Keeps that `workload` executed `step`.
    pub fn stepped(&mut self, workload: &str, step: Step) -> std::io::Result<()> {
        self.done.entry(workload.to_owned()).or_default().step = Some(step);
        self.save()
    }

    /// Keeps that `workload` performed power cycle `cycle`.
    pub fn cycled(&mut self, workload: &str, cycle: u64) -> std::io::Result<()> {
        let done = self.done.entry(workload.to_owned()).or_default();
        if done.cycle == Some(cycle) {
            return Ok(());
        }
        done.cycle = Some(cycle);
        self.save()
    }

    /// Keeps how far `workload`'s install has come.
    pub fn installed(&mut self, workload: &str, install: Install) -> std::io::Result<()> {
        let done = self.done.entry(workload.to_owned()).or_default();
        if done.install.as_ref() == Some(&install) {
            return Ok(());
        }
        done.install = Some(install);
        self.save()
    }

    /// Forgets `workload`: it is retired, or no longer on this node.
    pub fn forget(&mut self, workload: &str) -> std::io::Result<()> {
        if self.done.remove(workload).is_some() {
            return self.save();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_survives_a_restart_and_a_stranger_line_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress");
        let mut p = Progress::open(&path).unwrap();
        let step = Step { plan: "pl_1".into(), index: 2, failed: false };
        p.stepped("wl_1", step.clone()).unwrap();
        p.cycled("wl_1", 3).unwrap();
        let install = Install { digest: "ab12".into(), done: 4, lost: 1 };
        p.installed("wl_1", install.clone()).unwrap();
        let again = Progress::open(&path).unwrap();
        let all = Done { step: Some(step), cycle: Some(3), install: Some(install) };
        assert_eq!(again.of("wl_1"), all);
        std::fs::write(&path, "step wl_1 pl_1 x ok\n").unwrap();
        assert!(Progress::open(&path).is_err());
    }
}
