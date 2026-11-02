// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the reconciler has done that a restart must not do again, kept in a small text file
//! replaced atomically: for each workload, the last plan step it executed and how it turned out,
//! the power cycle it last performed (each cycle once), and how
//! far its install has come, which is kept per install rather than per plan, so a new plan that
//! runs the same install resumes it at the action that failed. A step's completion is journalled
//! before it is kept here, so a crash between the two runs the step again, which is a no-op that
//! reports success, rather than losing the completion. It also keeps the copies a migration's
//! destination received into volumes of its own, until the workload moved here or they were
//! deleted, so an abandoned migration's copies are deleted even across a restart. Beside it, one
//! file per install with provider steps keeps what they resolved to.

use buffa::Message as _;
use korpis_proto::korpis::v1 as pb;
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
    /// The volumes a migration's copies were received into here, while the workload has not
    /// moved here.
    pub received: Vec<String>,
    /// The state volume a live migration of it made here as its source, until it is deleted: it
    /// holds the guest's memory, so it is deleted whatever this agent forgot in a restart.
    pub state: Option<String>,
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
            ["received", w, volume] => {
                done.entry((*w).to_owned()).or_default().received.push((*volume).to_owned());
            }
            ["state", w, volume] => {
                done.entry((*w).to_owned()).or_default().state = Some((*volume).to_owned());
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
            lines.extend(d.received.iter().map(|v| format!("received {w} {v}")));
            lines.extend(d.state.iter().map(|v| format!("state {w} {v}")));
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

    /// Keeps how far `workload`'s install has come; what another install of it kept goes.
    pub fn installed(&mut self, workload: &str, install: Install) -> std::io::Result<()> {
        let done = self.done.entry(workload.to_owned()).or_default();
        if done.install.as_ref() == Some(&install) {
            return Ok(());
        }
        let digest = install.digest.clone();
        let replaced = done.install.replace(install).filter(|i| i.digest != digest);
        self.save()?;
        replaced.map_or(Ok(()), |old| self.drop_answers(&old.digest))
    }

    /// Keeps that `volumes` hold copies a migration of `workload` sends here, or, empty, that they
    /// are the workload's own now.
    pub fn receiving(&mut self, workload: &str, volumes: Vec<String>) -> std::io::Result<()> {
        let done = self.done.entry(workload.to_owned()).or_default();
        if done.received == volumes {
            return Ok(());
        }
        done.received = volumes;
        self.save()
    }

    /// Keeps that `state`, if any, is the state volume a live migration of `workload` made here.
    pub fn stating(&mut self, workload: &str, state: Option<String>) -> std::io::Result<()> {
        let done = self.done.entry(workload.to_owned()).or_default();
        if done.state == state {
            return Ok(());
        }
        done.state = state;
        self.save()
    }

    /// Each workload a live migration's state volume is kept here for, with that volume.
    #[must_use]
    pub fn states(&self) -> Vec<(String, String)> {
        let held = self.done.iter().filter_map(|(w, d)| Some((w.clone(), d.state.clone()?)));
        held.collect()
    }

    /// Each workload whose migration's copies are kept here, with the volumes holding them.
    #[must_use]
    pub fn received(&self) -> Vec<(String, Vec<String>)> {
        let held = self.done.iter().filter(|(_, d)| !d.received.is_empty());
        held.map(|(w, d)| (w.clone(), d.received.clone())).collect()
    }

    /// Forgets `workload`: it is retired, or no longer on this node. A state volume kept for it is
    /// kept until it is deleted.
    pub fn forget(&mut self, workload: &str) -> std::io::Result<()> {
        if let Some(done) = self.done.remove(workload) {
            if let Some(state) = done.state.clone() {
                let left = Done { state: Some(state), ..Done::default() };
                self.done.insert(workload.to_owned(), left);
            }
            self.save()?;
            return done.install.map_or(Ok(()), |i| self.drop_answers(&i.digest));
        }
        Ok(())
    }

    /// Where the answers to install `digest`'s provider steps are kept, beside the progress: a
    /// worker resumed at an action runs the same actions it started with, whatever a channel holds
    /// by then.
    fn answers_path(&self, digest: &str) -> PathBuf {
        self.path.with_file_name(format!("install-{digest}.steps"))
    }

    /// The answers kept for install `digest`, in the order of its provider actions.
    pub fn answers(&self, digest: &str) -> std::io::Result<Option<Vec<pb::StepResult>>> {
        let bytes = match std::fs::read(self.answers_path(digest)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut rest = bytes.as_slice();
        let mut answers = Vec::new();
        while let Some((len, tail)) = rest.split_first_chunk::<4>() {
            let len = usize::try_from(u32::from_le_bytes(*len)).unwrap_or(usize::MAX);
            let (one, tail) = tail.split_at_checked(len).ok_or_else(|| invalid("a cut answer"))?;
            answers.push(pb::StepResult::decode_from_slice(one).map_err(|_| invalid("an answer"))?);
            rest = tail;
        }
        if !rest.is_empty() {
            return Err(invalid("a cut answer"));
        }
        Ok(Some(answers))
    }

    /// Keeps `answers` for install `digest`.
    pub fn keep_answers(&self, digest: &str, answers: &[pb::StepResult]) -> std::io::Result<()> {
        let mut bytes = Vec::new();
        for a in answers {
            let one = a.encode_to_vec();
            let len = u32::try_from(one.len()).map_err(|_| invalid("an answer too large"))?;
            bytes.extend_from_slice(&len.to_le_bytes());
            bytes.extend_from_slice(&one);
        }
        crate::durable::replace(&self.answers_path(digest), &bytes)
    }

    /// Drops the answers kept for install `digest`: it ended, or another replaced it.
    pub fn drop_answers(&self, digest: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.answers_path(digest)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_kept_per_install_and_go_when_another_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Progress::open(&dir.path().join("progress")).unwrap();
        assert_eq!(p.answers("ab12").unwrap(), None);
        let one =
            pb::StepResult { resolved: "manifest 1".into(), pinned: true, ..Default::default() };
        let two = pb::StepResult { resolved: "public at 2".into(), ..Default::default() };
        p.keep_answers("ab12", &[one.clone(), two.clone()]).unwrap();
        assert_eq!(p.answers("ab12").unwrap(), Some(vec![one, two]));
        p.installed("wl_1", Install { digest: "ab12".into(), done: 1, lost: 0 }).unwrap();
        p.installed("wl_1", Install { digest: "ab12".into(), done: 2, lost: 0 }).unwrap();
        assert!(p.answers("ab12").unwrap().is_some(), "the same install keeps them");
        p.installed("wl_1", Install { digest: "cd34".into(), done: 0, lost: 0 }).unwrap();
        assert_eq!(p.answers("ab12").unwrap(), None, "another install of it replaces them");
        p.keep_answers("cd34", &[]).unwrap();
        p.forget("wl_1").unwrap();
        assert_eq!(p.answers("cd34").unwrap(), None, "and a forgotten workload's go with it");
    }

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
        p.receiving("wl_1", vec!["wl_1.data".into()]).unwrap();
        p.stating("wl_1", Some("wl_1.live_state".into())).unwrap();
        let again = Progress::open(&path).unwrap();
        let received = vec!["wl_1.data".to_owned()];
        let state = Some("wl_1.live_state".to_owned());
        let all =
            Done { step: Some(step), cycle: Some(3), install: Some(install), received, state };
        assert_eq!(again.of("wl_1"), all);
        std::fs::write(&path, "step wl_1 pl_1 x ok\n").unwrap();
        assert!(Progress::open(&path).is_err());
    }
}
