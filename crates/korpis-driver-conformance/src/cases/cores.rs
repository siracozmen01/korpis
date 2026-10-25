// SPDX-License-Identifier: Apache-2.0

//! Dedicated cores (§4.1 and §4.2 of docs/04-runtimes.md, §5.2 of docs/05-scheduling.md): a
//! driver that declares them held runs an object given a core on that core's CPUs alone, seen
//! from inside as the CPUs it may run on (on a kernel of its own, as many as it holds) and from
//! outside as its cgroup under the parent's `dedicated` one, with the parent's `shared` cgroup off
//! them; and it refuses, before it makes anything, a core it may not give.

use super::Stopped;
use crate::case::Verdict;
use crate::cores::{self, Held};
use crate::probe;
use crate::suite::Suite;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;
use std::time::Duration;

/// Whether the driver declares dedicated cores held, and its cgroup parent; why the case does not
/// apply, if not.
fn declared(s: &Suite) -> Result<String, Verdict> {
    let caps = s.capabilities();
    if !caps.enforcement.get("dedicated_cores").is_some_and(|e| e.enforced) {
        return Err(Verdict::NotApplicable("it declares dedicated_cores not held".into()));
    }
    if caps.cgroup_parent.is_empty() {
        return Err(Verdict::fail("it declares dedicated cores held and no cgroup parent"));
    }
    Ok(caps.cgroup_parent)
}

/// What the probe said of the CPUs it may run on.
fn cpus_said(output: &[u8]) -> Option<BTreeSet<u32>> {
    let text = String::from_utf8_lossy(output);
    let list = text.lines().rev().find_map(|l| l.strip_prefix("probe: cpus "))?;
    cores::cpu_list(list)
}

/// Whether the CPUs an object sees, `seen`, are those it holds: the same CPUs where it shares the
/// host's kernel, and as many where it has a kernel of its own, whose CPUs are its own.
fn as_held(s: &Suite, seen: &BTreeSet<u32>, held: &BTreeSet<u32>) -> bool {
    match s.tier() {
        pb::IsolationTier::ISOLATION_TIER_MICROVM | pb::IsolationTier::ISOLATION_TIER_VM => {
            seen.len() == held.len()
        }
        _ => seen == held,
    }
}

/// An object given the machine's last core runs on its CPUs alone, inside and outside.
pub async fn held(s: &Suite) -> Result<Verdict, Stopped> {
    let parent = match declared(s) {
        Ok(parent) => parent,
        Err(not) => return Ok(not),
    };
    let Some(core) = cores::last_core() else {
        return Ok(Verdict::NotApplicable("this machine has no core besides CPU 0's".into()));
    };
    let cpus: BTreeSet<u32> = core.cpus.iter().copied().collect();
    let kept = Held::hold(&parent, &cpus).map_err(|e| format!("the cores' cpusets: {e}"))?;
    let mut spec = s.prepared(&probe::SERVE).await?;
    spec.cores = vec![core];
    let id = s.create(&spec).await?;
    s.driver().start(&id).await.map_err(|e| format!("Start: {e}"))?;
    s.serving(&id, &probe::SERVE).await?;
    let outside = cores::holding(&parent, &cpus);
    let inside = if s.capabilities().exec {
        let quick = (Duration::from_secs(30), 4096);
        let asked = s.driver().exec(&id, &probe::CPUS, quick).await;
        let (output, _) = asked.map_err(|e| format!("Exec of {:?}: {e}", probe::CPUS))?;
        Some(cpus_said(&output).ok_or("the probe did not say its CPUs")?)
    } else {
        None
    };
    // The object ends before its cpusets are put back, as a node frees a core.
    let _ = s.driver().kill(&id).await;
    let _ = s.driver().destroy(&id).await;
    drop(kept);
    let held = cores::listed(&cpus);
    if let Err(why) = outside {
        return Ok(Verdict::fail(format!("holding CPUs {held}: {why}")));
    }
    Ok(match inside {
        Some(seen) => Verdict::check(
            as_held(s, &seen, &cpus),
            format!("holding CPUs {held}, it may run on CPUs {}", cores::listed(&seen)),
        ),
        None => Verdict::check(true, format!("holding CPUs {held}; no exec to look inside")),
    })
}

/// An object given the core of CPU 0, which no node offers, is refused at `Create`.
pub async fn refuses_cpu_0s(s: &Suite) -> Result<Verdict, Stopped> {
    if let Err(not) = declared(s) {
        return Ok(not);
    }
    let mut spec = s.prepared(&probe::SERVE).await?;
    spec.cores = vec![pb::Core { id: 0, cpus: vec![0], numa: 0, ..Default::default() }];
    Ok(match s.create(&spec).await {
        Ok(id) => Verdict::fail(format!("it made {id} holding the core of CPU 0")),
        Err(e) => Verdict::check(true, format!("refused: {e}")),
    })
}
