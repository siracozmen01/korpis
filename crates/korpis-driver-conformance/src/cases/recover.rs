// SPDX-License-Identifier: Apache-2.0

//! `Recover`, which §4.2 of docs/04-runtimes.md says is not optional: a driver killed with its
//! objects in every state finds each of them again once it is started, with its labels and as it
//! was, and goes on driving them (§4.4 of docs/02-architecture.md); an object whose kernel ended
//! it for its memory says so, and is found again stopped, never running; and after the host has
//! restarted, none of them runs, each is still held, and each starts again. A driver that adopts
//! after a clean restart and not after a crash fails only in the situation that matters (§8).

use super::Stopped;
use crate::case::Verdict;
use crate::probe;
use crate::suite::{CREATED, RUNNING, STOPPED, Suite, state};
use korpis_proto::korpis::driver::v1 as driver;
use std::time::{Duration, Instant};

/// How long an object is given to be seen where it is going.
const SEEN: Duration = Duration::from_secs(60);

/// What `Recover` says of `id`, if it lists it.
async fn listed(s: &Suite, id: &str) -> Result<Option<driver::RuntimeObject>, Stopped> {
    let objects = s.driver().recover().await.map_err(|e| format!("Recover: {e}"))?;
    Ok(objects.into_iter().find(|o| o.runtime_id == id))
}

/// Whether the driver says it stopped `id` in an event, within ten seconds.
async fn told_stopped(s: &Suite, id: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if s.driver().events_for(id).iter().any(|e| state(e) == STOPPED) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// An object that ran, one that exited 3, and one created are each listed again, with their
/// labels and as they were, by the driver started again after a `SIGKILL`; and the one running
/// is stopped through it, its stop reaching the entry and sent as an event.
pub async fn adopts_after_a_kill(s: &Suite) -> Result<Verdict, Stopped> {
    let (running, running_spec) = s.running(&probe::SERVE).await?;
    let (exited, exited_spec) = s.created(&probe::EXIT_3).await?;
    s.driver().start(&exited).await.map_err(|e| format!("Start: {e}"))?;
    s.until(&exited, SEEN, |st| state(st) == STOPPED).await?;
    let (created, created_spec) = s.created(&probe::SERVE).await?;
    let before = s.driver().inspect(&running).await.map_err(|e| format!("Inspect: {e}"))?;
    s.restart(None).await?;
    let wanted = [
        (&running, &running_spec, RUNNING),
        (&exited, &exited_spec, STOPPED),
        (&created, &created_spec, CREATED),
    ];
    for (id, spec, was) in wanted {
        let Some(o) = listed(s, id).await? else {
            return Ok(Verdict::fail(format!("restarted, it no longer lists {id}")));
        };
        if o.labels.as_option() != spec.labels.as_option() {
            return Ok(Verdict::fail(format!("{id}'s labels came back as {:?}", o.labels)));
        }
        let status = o.status.as_option().cloned().unwrap_or_default();
        if state(&status) != was {
            return Ok(Verdict::fail(format!("{id} was {was:?}, and came back {status:?}")));
        }
    }
    let after = s.driver().inspect(&running).await.map_err(|e| format!("Inspect: {e}"))?;
    if after.started_at != before.started_at || after.pid != before.pid {
        return Ok(Verdict::fail(format!(
            "adopted, it is another run: {before:?}, then {after:?}"
        )));
    }
    let exit = s.driver().inspect(&exited).await.map_err(|e| format!("Inspect: {e}"))?;
    if exit.exit_code != Some(3) {
        return Ok(Verdict::fail(format!("adopted, the exit of 3 is now {exit:?}")));
    }
    s.driver().stop(&running, SEEN).await.map_err(|e| format!("Stop, adopted: {e}"))?;
    let stopped = s.until(&running, SEEN, |st| state(st) == STOPPED).await?;
    if stopped.exit_code != Some(0) {
        return Ok(Verdict::fail(format!("adopted, its stop did not reach it: {stopped:?}")));
    }
    Ok(Verdict::check(told_stopped(s, &running).await, "adopted, its stop was sent no event"))
}

/// An object its kernel ended for exceeding its memory says `oom_killed`, and is found stopped,
/// still saying so, by the driver started again after a `SIGKILL`.
pub async fn says_its_memory_ended_it(s: &Suite) -> Result<Verdict, Stopped> {
    let (id, _) = s.created(&probe::ALLOC).await?;
    s.driver().start(&id).await.map_err(|e| format!("Start: {e}"))?;
    let status = s.until(&id, SEEN, |st| state(st) == STOPPED).await?;
    if status.reason != "oom_killed" || status.exit_code == Some(0) {
        return Ok(Verdict::fail(format!("ended for its memory, it says {status:?}")));
    }
    s.restart(None).await?;
    let Some(o) = listed(s, &id).await? else {
        return Ok(Verdict::fail("restarted, the driver no longer lists it"));
    };
    let after = o.status.as_option().cloned().unwrap_or_default();
    let same = state(&after) == STOPPED && after.reason == "oom_killed";
    Ok(Verdict::check(same, format!("restarted, it came back {after:?}")))
}

/// After the host restarts, as the fixture restarts it, two objects that ran are held and
/// neither runs, and one starts again and stops as it did before.
pub async fn a_restarted_host_runs_none(s: &Suite) -> Result<Verdict, Stopped> {
    if s.fixture.host_restart.is_none() {
        return Ok(Verdict::NotApplicable("the fixture names no restart of the host".into()));
    }
    let (first, _) = s.running(&probe::SERVE).await?;
    let (second, _) = s.running(&probe::SERVE).await?;
    let mut pids = Vec::new();
    for id in [&first, &second] {
        let status = s.driver().inspect(id).await.map_err(|e| format!("Inspect: {e}"))?;
        if status.pid == 0 {
            return Ok(Verdict::fail(format!("running, {id} says no process: {status:?}")));
        }
        pids.push(status.pid);
    }
    s.restart(Some(&pids)).await?;
    let unknown = Some(driver::RuntimeState::RUNTIME_STATE_UNKNOWN);
    for id in [&first, &second] {
        if listed(s, id).await?.is_none() {
            return Ok(Verdict::fail(format!("after the host's restart, {id} is not held")));
        }
        let ended = |st: &driver::RuntimeStatus| state(st) != RUNNING && state(st) != unknown;
        if let Err(e) = s.until(id, SEEN, ended).await {
            return Ok(Verdict::fail(format!("after the host's restart, {id}: {e}")));
        }
    }
    s.driver().start(&first).await.map_err(|e| format!("Start, again: {e}"))?;
    s.serving(&first, &probe::SERVE).await?;
    s.driver().stop(&first, SEEN).await.map_err(|e| format!("Stop, again: {e}"))?;
    let stopped = s.until(&first, SEEN, |st| state(st) == STOPPED).await?;
    Ok(Verdict::check(stopped.exit_code == Some(0), format!("started again: {stopped:?}")))
}
