// SPDX-License-Identifier: AGPL-3.0-or-later

//! An install's provider actions, resolved before its worker starts (§4 of docs/09-recipes.md).
//! Each is asked of the control plane, which calls the step provider the intent pinned, and the
//! install waits until every one is answered. The answers are kept beside the progress, so the
//! worker, and a worker resumed at an action, runs one list: the step's own actions with each
//! provider action replaced by what it resolved to. A file becomes a `fetch` to `to/path` and a
//! `chmod` to its mode; an archive is fetched into the content store and unpacked at `to/path` by
//! an `extract` that reads what fetches downloaded, never a recipe's file (Finding 49).

use super::{Reconciler, failed};
use crate::host::Host;
use crate::reconcile::act::Failure;
use crate::runtime::Runtime;
use crate::steps::Unresolved;
use korpis_core::recipe::install::{FETCHED, Hash, InstallStep};
use korpis_core::recipe::installing::{InstallAction, detail};
use korpis_proto::korpis::v1 as pb;

/// What a worker runs, and what the node fetches into its content store first: each a URL and the
/// digest it must have.
pub(super) struct Expanded {
    pub(super) actions: Vec<pb::Value>,
    pub(super) fetches: Vec<(String, String)>,
}

fn wire(step: InstallStep) -> pb::Value {
    korpis_wire::value::to_pb(&detail::encode(&InstallAction { step, context: None }))
}

/// The actions provider step `to` resolved to as `answer`, and the content to fetch for them.
fn resolved(to: &str, answer: &pb::StepResult, out: &mut Expanded) -> Result<(), Failure> {
    let result = korpis_wire::step::step_result_from_pb(answer);
    if let Some(why) = result.problem() {
        return Err(failed(format!("a provider's answer: {why}")));
    }
    for a in result.artifacts {
        let at = format!("{}/{}", to.trim_end_matches('/'), a.path);
        out.fetches.push((a.url.clone(), format!("sha256:{}", a.sha256)));
        if a.extract {
            let from = format!("{FETCHED}sha256/{}", a.sha256);
            out.actions.push(wire(InstallStep::Extract {
                from,
                to: at,
                strip: 0,
                include: vec![],
            }));
            continue;
        }
        let hash = Hash::Sha256(a.sha256);
        out.actions.push(wire(InstallStep::Fetch { url: a.url, hash, to: at.clone() }));
        if a.mode != 0 {
            out.actions.push(wire(InstallStep::Chmod { path: at, mode: a.mode }));
        }
    }
    Ok(())
}

/// Whether `a` is a provider step.
fn provider(a: &InstallAction) -> bool {
    matches!(a.step, InstallStep::Provider { .. })
}

/// `wire_actions`, decoded as `decoded`, with each provider action replaced by what `answers`
/// says it resolved to, in order.
pub(super) fn expand(
    wire_actions: &[pb::Value],
    decoded: &[InstallAction],
    answers: &[pb::StepResult],
) -> Result<Expanded, Failure> {
    let mut out = Expanded { actions: Vec::new(), fetches: Vec::new() };
    let mut answers = answers.iter();
    for (w, d) in wire_actions.iter().zip(decoded) {
        match &d.step {
            InstallStep::Provider { to, .. } => {
                let answer = answers.next().ok_or_else(|| failed("a provider step unanswered"))?;
                resolved(to, answer, &mut out)?;
            }
            InstallStep::Fetch { url, hash, .. } => {
                out.fetches.push((url.clone(), hash.to_digest()));
                out.actions.push(w.clone());
            }
            _ => out.actions.push(w.clone()),
        }
    }
    Ok(out)
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// What install `digest`'s provider actions resolved to, kept or asked for; `None` while one
    /// is awaited. `(plan, s)` is the install step, `decoded` its actions.
    pub(super) fn answers(
        &mut self,
        (plan, s): (&str, &pb::Step),
        digest: &str,
        decoded: &[InstallAction],
    ) -> Result<Option<Vec<pb::StepResult>>, Failure> {
        let wanted = decoded.iter().filter(|a| provider(a)).count();
        let kept = self.progress.answers(digest).map_err(|e| failed(format!("answers: {e}")))?;
        if let Some(kept) = kept.filter(|k| k.len() == wanted) {
            return Ok(Some(kept));
        }
        let (os, arch) = korpis_core::content::this_machine();
        let platform = pb::Platform { os: os.into(), arch: arch.into(), ..Default::default() };
        let mut answers = Vec::new();
        for (i, _) in decoded.iter().enumerate().filter(|(_, a)| provider(a)) {
            let action = u32::try_from(i).map_err(|_| failed("too many actions"))?;
            match self.wiring.steps.poll((plan, s.index, action), &platform) {
                Some(Ok(r)) => answers.push(r),
                Some(Err(Unresolved::Refused(why))) => {
                    self.wiring.steps.settled(plan, s.index);
                    return Err(failed(format!("install action {i} did not resolve: {why}")));
                }
                Some(Err(Unresolved::Later(why))) => {
                    tracing::info!(plan, action, %why, "a provider step resolves later");
                }
                None => {}
            }
        }
        if answers.len() < wanted {
            return Ok(None);
        }
        self.progress
            .keep_answers(digest, &answers)
            .map_err(|e| failed(format!("answers: {e}")))?;
        self.wiring.steps.settled(plan, s.index);
        Ok(Some(answers))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        reason = "a failing test should fail loudly"
    )]

    use super::*;
    use korpis_core::value::ValueMap;

    fn decoded_of(actions: &[pb::Value]) -> Vec<InstallAction> {
        actions
            .iter()
            .map(|v| detail::decode(&korpis_wire::value::from_pb("a", v).unwrap()).unwrap())
            .collect()
    }

    fn artifact(path: &str, extract: bool, mode: u32) -> pb::StepArtifact {
        pb::StepArtifact {
            path: path.into(),
            url: format!("https://cdn.example/{path}"),
            sha256: "b".repeat(64),
            size: 1,
            mode,
            extract,
            ..Default::default()
        }
    }

    #[test]
    fn a_provider_step_becomes_fetches_and_extracts_beneath_its_to() {
        let steam = InstallStep::Provider {
            provider: "valve/steam.app".into(),
            to: "/data/server".into(),
            args: ValueMap::new(),
        };
        let eula =
            InstallStep::Write { path: "/data/eula.txt".into(), content: "x".into(), mode: 0o644 };
        let wire_actions = vec![wire(eula.clone()), wire(steam)];
        let decoded = decoded_of(&wire_actions);
        let answer = pb::StepResult {
            artifacts: vec![artifact("game.tar", true, 0), artifact("bin/srcds", false, 0o755)],
            ..Default::default()
        };
        let e = expand(&wire_actions, &decoded, &[answer]).unwrap();
        let steps: Vec<InstallStep> = decoded_of(&e.actions).into_iter().map(|a| a.step).collect();
        let hex = "b".repeat(64);
        assert_eq!(steps[0], eula, "the step's own actions stay");
        assert_eq!(
            steps[1],
            InstallStep::Extract {
                from: format!("/.korpis/content/fetched/sha256/{hex}"),
                to: "/data/server/game.tar".into(),
                strip: 0,
                include: vec![],
            }
        );
        let fetched = InstallStep::Fetch {
            url: "https://cdn.example/bin/srcds".into(),
            hash: Hash::Sha256(hex.clone()),
            to: "/data/server/bin/srcds".into(),
        };
        assert_eq!(steps[2], fetched);
        assert_eq!(
            steps[3],
            InstallStep::Chmod { path: "/data/server/bin/srcds".into(), mode: 0o755 }
        );
        assert_eq!(e.fetches.len(), 2, "both are in the content store before the worker starts");

        let climbing =
            pb::StepResult { artifacts: vec![artifact("../etc", false, 0)], ..Default::default() };
        assert!(expand(&wire_actions, &decoded, &[climbing]).is_err(), "checked again here");
        assert!(expand(&wire_actions, &decoded, &[]).is_err(), "every provider step answered");
    }
}
