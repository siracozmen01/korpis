// SPDX-License-Identifier: Apache-2.0

//! How an object ends (§4.2 of docs/04-runtimes.md): `Stop` reaches its entry with the driver's
//! graceful default and kills it only once its grace is over, `Kill` ends it at once, an entry
//! that exits is inspected with its status and sent as an event, `Destroy` releases it so the
//! driver no longer holds it, and each of these asked again of an object already where it asks
//! leaves it there.

use super::Stopped;
use crate::case::Verdict;
use crate::probe;
use crate::suite::{STOPPED, Suite, state};
use connectrpc::ErrorCode;
use std::time::{Duration, Instant};

/// How long an object is given to be seen stopped once it was asked to stop.
const ENDING: Duration = Duration::from_secs(60);

/// A `Stop` reaches the probe, which exits 0 well within its grace; a second `Stop` changes
/// nothing.
pub async fn stop_reaches_the_entry(s: &Suite) -> Result<Verdict, Stopped> {
    let (id, _) = s.running(&probe::SERVE).await?;
    let grace = Duration::from_secs(60);
    let asked = Instant::now();
    s.driver().stop(&id, grace).await.map_err(|e| format!("Stop: {e}"))?;
    let status = s.until(&id, ENDING, |st| state(st) == STOPPED).await?;
    if asked.elapsed() >= grace {
        return Ok(Verdict::fail("it was killed at the end of its grace, not stopped by it"));
    }
    if status.exit_code != Some(0) {
        return Ok(Verdict::fail(format!("the probe exits 0 when stopped: {status:?}")));
    }
    if let Err(e) = s.driver().stop(&id, grace).await {
        return Ok(Verdict::fail(format!("a second Stop: {e}")));
    }
    let after = s.driver().inspect(&id).await.map_err(|e| format!("Inspect: {e}"))?;
    Ok(Verdict::check(after == status, format!("a second Stop changed it: {after:?}")))
}

/// A probe that ignores the graceful default is killed once its grace is over, and not before.
pub async fn stop_kills_after_grace(s: &Suite) -> Result<Verdict, Stopped> {
    let (id, _) = s.running(&probe::STUBBORN).await?;
    let grace = Duration::from_secs(3);
    let asked = Instant::now();
    s.driver().stop(&id, grace).await.map_err(|e| format!("Stop: {e}"))?;
    let status = s.until(&id, ENDING, |st| state(st) == STOPPED).await?;
    if asked.elapsed() < grace {
        return Ok(Verdict::fail(format!(
            "it was ended after {:?}, within its grace",
            asked.elapsed()
        )));
    }
    Ok(Verdict::check(status.exit_code != Some(0), format!("killed, it exited 0: {status:?}")))
}

/// A `Kill` ends a running object; a second `Kill` changes nothing.
pub async fn kill(s: &Suite) -> Result<Verdict, Stopped> {
    let (id, _) = s.running(&probe::SERVE).await?;
    s.driver().kill(&id).await.map_err(|e| format!("Kill: {e}"))?;
    let status = s.until(&id, ENDING, |st| state(st) == STOPPED).await?;
    if let Err(e) = s.driver().kill(&id).await {
        return Ok(Verdict::fail(format!("a second Kill: {e}")));
    }
    let after = s.driver().inspect(&id).await.map_err(|e| format!("Inspect: {e}"))?;
    Ok(Verdict::check(after == status, format!("a second Kill changed it: {after:?}")))
}

/// An entry that exits 3 is inspected stopped with status 3, and an event says so.
pub async fn exit_is_reported(s: &Suite) -> Result<Verdict, Stopped> {
    let (id, _) = s.created(&probe::EXIT_3).await?;
    s.driver().start(&id).await.map_err(|e| format!("Start: {e}"))?;
    let status = s.until(&id, ENDING, |st| state(st) == STOPPED).await?;
    if status.exit_code != Some(3) {
        return Ok(Verdict::fail(format!("the probe exited 3: {status:?}")));
    }
    if status.finished_at.is_unset() {
        return Ok(Verdict::fail(format!("it does not say when it ended: {status:?}")));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if s.driver().events_for(&id).iter().any(|e| state(e) == STOPPED) {
            return Ok(Verdict::Pass);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(Verdict::fail("no event said it stopped"))
}

/// A destroyed object is no longer held: inspected, listed, or destroyed again.
pub async fn destroy_releases(s: &Suite) -> Result<Verdict, Stopped> {
    let (id, _) = s.running(&probe::SERVE).await?;
    s.driver().kill(&id).await.map_err(|e| format!("Kill: {e}"))?;
    s.until(&id, ENDING, |st| state(st) == STOPPED).await?;
    s.driver().destroy(&id).await.map_err(|e| format!("Destroy: {e}"))?;
    let not_found =
        |e: Option<crate::client::Refused>| e.is_some_and(|e| e.code == ErrorCode::NotFound);
    if !not_found(s.driver().inspect(&id).await.err()) {
        return Ok(Verdict::fail("destroyed, it is still inspected"));
    }
    let objects = s.driver().recover().await.map_err(|e| format!("Recover: {e}"))?;
    if objects.iter().any(|o| o.runtime_id == id) {
        return Ok(Verdict::fail("destroyed, it is still listed by Recover"));
    }
    Ok(Verdict::check(
        not_found(s.driver().destroy(&id).await.err()),
        "a second Destroy was not NotFound",
    ))
}
