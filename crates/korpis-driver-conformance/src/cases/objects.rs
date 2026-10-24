// SPDX-License-Identifier: Apache-2.0

//! `Create`, `Start`, and what a driver answers of an object it does not hold (§4.2 of
//! docs/04-runtimes.md): an object is made before anything runs in it, made once however often
//! it is asked for, refused when its spec names a volume the helper did not make (§6.1 of
//! docs/02-architecture.md), and started once however often it is asked; and every verb naming an
//! object the driver does not hold is answered `NotFound`, which the node takes as "already gone".

use super::Stopped;
use crate::case::Verdict;
use crate::probe;
use crate::suite::{CREATED, RUNNING, Suite, state};
use connectrpc::ErrorCode;
use std::time::Duration;

/// A created object has not started.
pub async fn create_runs_nothing(s: &Suite) -> Result<Verdict, Stopped> {
    let (id, _) = s.created(&probe::SERVE).await?;
    let status = s.driver.inspect(&id).await.map_err(|e| format!("Inspect: {e}"))?;
    if state(&status) != CREATED {
        return Ok(Verdict::fail(format!("created, it is {status:?}")));
    }
    Ok(Verdict::check(status.started_at.is_unset(), format!("it says it started: {status:?}")))
}

/// A second `Create` of the same spec answers the same object, and makes no other.
pub async fn create_again_is_the_same(s: &Suite) -> Result<Verdict, Stopped> {
    let (id, spec) = s.created(&probe::SERVE).await?;
    let again = s.driver.create(&spec).await.map_err(|e| format!("the second Create: {e}"))?;
    if again != id {
        return Ok(Verdict::fail(format!("a second object, {again}, beside {id}")));
    }
    let objects = s.driver.recover().await.map_err(|e| format!("Recover: {e}"))?;
    let workload = spec.labels.as_option().map(|l| l.workload.clone()).unwrap_or_default();
    let of =
        objects.iter().filter(|o| o.labels.as_option().is_some_and(|l| l.workload == workload));
    Ok(Verdict::check(of.count() == 1, "Recover lists more than one object of the workload"))
}

/// A spec whose volume is not under the volume root is refused, and nothing is made of it.
pub async fn refuses_a_volume_outside(s: &Suite) -> Result<Verdict, Stopped> {
    let mut spec = s.prepared(&probe::SERVE).await?;
    let Some(mount) = spec.mounts.first_mut() else {
        return Ok(Verdict::fail("the probe's volume was not prepared as a mount"));
    };
    mount.source = "/etc".into();
    if let Ok(id) = s.driver.create(&spec).await {
        let _ = s.driver.destroy(&id).await;
        return Ok(Verdict::fail("an object was made over /etc"));
    }
    let objects = s.driver.recover().await.map_err(|e| format!("Recover: {e}"))?;
    let workload = spec.labels.as_option().map(|l| l.workload.clone()).unwrap_or_default();
    let left = objects.iter().any(|o| o.labels.as_option().is_some_and(|l| l.workload == workload));
    Ok(Verdict::check(!left, "the refused spec left an object behind"))
}

/// A started object runs, says when it started, and a second `Start` leaves it running.
pub async fn start_runs_it(s: &Suite) -> Result<Verdict, Stopped> {
    let (id, _) = s.running(&probe::SERVE).await?;
    let status = s.driver.inspect(&id).await.map_err(|e| format!("Inspect: {e}"))?;
    if status.started_at.is_unset() {
        return Ok(Verdict::fail(format!("running, it does not say when it started: {status:?}")));
    }
    if let Err(e) = s.driver.start(&id).await {
        return Ok(Verdict::fail(format!("a second Start: {e}")));
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let after = s.driver.inspect(&id).await.map_err(|e| format!("Inspect: {e}"))?;
    if state(&after) != RUNNING || after.started_at != status.started_at {
        return Ok(Verdict::fail(format!("a second Start changed it: {after:?}")));
    }
    Ok(Verdict::Pass)
}

/// Every verb naming an object the driver does not hold is answered `NotFound`.
pub async fn unknown_is_not_found(s: &Suite) -> Result<Verdict, Stopped> {
    let none = "conformance-no-such-object";
    let d = &s.driver;
    let answers = [
        ("Start", d.start(none).await.err()),
        ("Stop", d.stop(none, Duration::from_secs(1)).await.err()),
        ("Kill", d.kill(none).await.err()),
        ("Destroy", d.destroy(none).await.err()),
        ("Inspect", d.inspect(none).await.err()),
        ("Stats", d.stats(none).await.err()),
        ("Signal", d.signal(none, "SIGTERM").await.err()),
    ];
    let wrong: Vec<String> = answers
        .into_iter()
        .filter(|(_, e)| e.as_ref().is_none_or(|e| e.code != ErrorCode::NotFound))
        .map(|(verb, e)| format!("{verb}: {}", e.map_or("answered".into(), |e| e.to_string())))
        .collect();
    Ok(Verdict::check(wrong.is_empty(), wrong.join("; ")))
}
