// SPDX-License-Identifier: Apache-2.0

//! Enforcement and capability honesty (§4.1 and §8 of docs/04-runtimes.md): every limit a driver
//! declares enforced is met inside one of its objects and found held where the kernel or the
//! hypervisor holds it, since a limit declared and not enforced is a lie in a text field (Rule
//! K-3); and a capability declared is one the protocol can exercise. The probe meets memory,
//! processes, and disk from inside, through `Exec`, saying how far it got; processor time is read
//! from the driver's own counters, since a guest's clock does not see the time it is not given.

use super::Stopped;
use crate::case::Verdict;
use crate::probe;
use crate::suite::{CPU, MEMORY, PIDS, Suite, VOLUME};
use std::time::{Duration, Instant};

/// How long a command meeting a limit is given.
const MEETING: Duration = Duration::from_secs(120);
/// The most of its output kept.
const OUTPUT: u32 = 64 << 10;
/// A mebibyte.
const MIB: u64 = 1 << 20;

/// Whether the driver declares the limit `key` enforced; why the case does not apply, if not.
fn declared(s: &Suite, key: &str) -> Result<(), Verdict> {
    let caps = s.capabilities();
    match caps.enforcement.get(key) {
        Some(e) if e.enforced && caps.exec => Ok(()),
        Some(e) if e.enforced => Err(Verdict::NotApplicable(format!(
            "it declares {key} enforced and no exec to meet it"
        ))),
        _ => Err(Verdict::NotApplicable(format!("it declares {key} not enforced"))),
    }
}

/// The number the last line of `output` beginning with `prefix` says next.
fn said(output: &[u8], prefix: &str) -> Option<u64> {
    let text = String::from_utf8_lossy(output);
    let line = text.lines().rev().find_map(|l| l.strip_prefix(prefix))?;
    line.split_whitespace().next()?.parse().ok()
}

/// Runs the probe's `command` through `Exec` in a new running object: what it wrote, and the
/// status it ended with.
async fn met(s: &Suite, command: &[&str]) -> Result<(Vec<u8>, u32), Stopped> {
    let (id, _) = s.running(&probe::SERVE).await?;
    let (output, exit) = s
        .driver()
        .exec(&id, command, (MEETING, OUTPUT))
        .await
        .map_err(|e| format!("Exec of {command:?}: {e}"))?;
    Ok((output, exit.status))
}

/// Memory taken past the limit is refused by the kernel's kill, after at most the limit and at
/// least a quarter of it.
pub async fn memory(s: &Suite) -> Result<Verdict, Stopped> {
    if let Err(not) = declared(s, "memory") {
        return Ok(not);
    }
    let (output, status) = met(s, &["/probe", "alloc"]).await?;
    let held = said(&output, "probe: holds ").unwrap_or(0);
    let limit = MEMORY.1 / MIB;
    if status != 137 {
        return Ok(Verdict::fail(format!("taking memory, it ended {status}, holding {held} MiB")));
    }
    let within = held <= limit && held >= limit / 4;
    Ok(Verdict::check(within, format!("killed holding {held} MiB, against a limit of {limit}")))
}

/// Threads started past the limit are refused, at most the limit and at least a quarter of it.
pub async fn pids(s: &Suite) -> Result<Verdict, Stopped> {
    if let Err(not) = declared(s, "pids") {
        return Ok(not);
    }
    let (output, _) = met(s, &probe::THREADS).await?;
    let Some(started) = said(&output, "probe: started ") else {
        return Ok(Verdict::fail(format!(
            "the probe said nothing: {}",
            String::from_utf8_lossy(&output)
        )));
    };
    let within = (PIDS / 4..PIDS).contains(&started);
    Ok(Verdict::check(within, format!("{started} threads started, against a limit of {PIDS}")))
}

/// A volume is filled to at most its size, and the write past it refused.
pub async fn disk(s: &Suite) -> Result<Verdict, Stopped> {
    if let Err(not) = declared(s, "disk") {
        return Ok(not);
    }
    let (output, _) = met(s, &probe::FILL).await?;
    let text = String::from_utf8_lossy(&output).into_owned();
    let Some(written) = said(&output, "probe: wrote ") else {
        return Ok(Verdict::fail(format!("the probe said nothing: {text}")));
    };
    let size = VOLUME / MIB;
    let refused = text.contains("No space left") || text.contains("quota");
    let within = written <= size && refused;
    Ok(Verdict::check(within, format!("{written} MiB written of a {size} MiB volume: {text}")))
}

/// An object kept busy on more threads than it may use is given at most its limit of processor
/// time, as the driver's own counters say, and at least half of it.
pub async fn cpu(s: &Suite) -> Result<Verdict, Stopped> {
    if !s.capabilities().enforcement.get("cpu").is_some_and(|e| e.enforced) {
        return Ok(Verdict::NotApplicable("it declares cpu not enforced".into()));
    }
    let (id, _) = s.running(&probe::SPIN).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let read = || async {
        let stats = s.driver().stats(&id).await.map_err(|e| format!("Stats: {e}"))?;
        let used = stats.counters.as_option().and_then(|c| c.cpu_usec);
        Ok::<_, Stopped>((Instant::now(), used))
    };
    let (from, before) = read().await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let (to, after) = read().await?;
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(Verdict::fail(
            "it declares cpu enforced, and its counters say no processor time",
        ));
    };
    let elapsed = u64::try_from(to.duration_since(from).as_micros()).unwrap_or(u64::MAX).max(1);
    let millis = after.saturating_sub(before).saturating_mul(1000) / elapsed;
    // A quota is kept per period, so a reading over a few seconds may run a tenth over it.
    let within = millis <= CPU.1 + CPU.1 / 10 && millis >= CPU.1 / 2;
    Ok(Verdict::check(
        within,
        format!("{millis} thousandths of a processor, against a limit of {}", CPU.1),
    ))
}

/// A driver that declares a resource resized without a restart declares what the protocol has no
/// verb to ask of it.
pub fn hot_resize(s: &Suite) -> Verdict {
    let declared = s.capabilities().hot_resize;
    if declared.is_empty() {
        return Verdict::NotApplicable("it declares no resource resized while it runs".into());
    }
    Verdict::fail(format!("it declares {declared:?} resized while it runs, which no verb asks for"))
}
