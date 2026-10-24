// SPDX-License-Identifier: Apache-2.0

//! Exec honesty (§8 of docs/04-runtimes.md): a driver that declares `exec` runs a command as its
//! object's own process runs, killed at its timeout and sending no more of its output than its
//! limit, as `Exec` says (§4.2); and one that does not declare it refuses it. The probe says who
//! it is, through `Exec` and as the entry wrote it, and the two are compared line by line.

use super::Stopped;
use crate::case::Verdict;
use crate::probe;
use crate::suite::Suite;
use std::time::{Duration, Instant};

/// How long a command that ends of itself is given.
const QUICK: Duration = Duration::from_secs(30);
/// The timeout a command that never ends is given.
const TIMEOUT: Duration = Duration::from_secs(2);
/// How long past its timeout a command may still be reported as running.
const LATE: Duration = Duration::from_secs(10);
/// The most of its output a command may send, as the protocol bounds it.
const MOST: u32 = 64 << 10;
/// The most of an identity kept.
const IDENTITY: u32 = 32 << 10;

/// Why the case does not apply to a driver that does not declare `exec`.
fn declared(s: &Suite) -> Result<(), Verdict> {
    if s.capabilities().exec {
        Ok(())
    } else {
        Err(Verdict::NotApplicable("it declares no exec".into()))
    }
}

/// What the probe's `command` says through `Exec` in `id`, which must exit 0 and send it whole.
async fn said(s: &Suite, id: &str, command: &[&str]) -> Result<String, Stopped> {
    let (output, exit) = s
        .driver()
        .exec(id, command, (QUICK, IDENTITY))
        .await
        .map_err(|e| format!("Exec of {command:?}: {e}"))?;
    let text = String::from_utf8_lossy(&output).trim_end().to_owned();
    if exit.status != 0 || exit.truncated || exit.timed_out {
        return Err(format!("Exec of {command:?} ended {exit:?}: {text}"));
    }
    Ok(text)
}

/// The lines of `first` and `second` that differ, each as both say it.
fn differing(first: &str, second: &str) -> Vec<String> {
    let (a, b): (Vec<&str>, Vec<&str>) = (first.lines().collect(), second.lines().collect());
    let mut differ: Vec<String> = a
        .iter()
        .zip(&b)
        .filter(|(x, y)| x != y)
        .map(|(x, y)| format!("the entry's \"{x}\", the command's \"{y}\""))
        .collect();
    if a.len() != b.len() {
        differ.push(format!("the entry says {} lines, the command {}", a.len(), b.len()));
    }
    differ
}

/// A command runs as the entry does: its user, groups, capabilities, and confinement; its
/// namespaces and cgroup; its limits; its working directory and environment.
pub async fn as_the_entry_runs(s: &Suite) -> Result<Verdict, Stopped> {
    if let Err(not) = declared(s) {
        return Ok(not);
    }
    let (id, _) = s.running(&probe::SERVE).await?;
    let entry = said(s, &id, &probe::ENTRY).await?;
    let command = said(s, &id, &probe::WHOAMI).await?;
    let unread = |line: &str| line.contains("unread");
    let read = ["Uid:", "Gid:", "CapEff:", "ns mnt:", "ns pid:", "ns user:", "cwd:"]
        .iter()
        .all(|field| entry.lines().any(|l| l.starts_with(field) && !unread(l)));
    if !read {
        return Ok(Verdict::fail(format!("the entry could not say who it is: {entry}")));
    }
    Ok(Verdict::check(entry == command, differing(&entry, &command).join("; ")))
}

/// A command that outlives its timeout, ignoring the terminating signal, is ended soon after it
/// and said to have timed out, and no longer runs.
pub async fn killed_at_its_timeout(s: &Suite) -> Result<Verdict, Stopped> {
    if let Err(not) = declared(s) {
        return Ok(not);
    }
    let (id, _) = s.running(&probe::SERVE).await?;
    let from = Instant::now();
    let asked = s.driver().exec(&id, &probe::STUBBORN, (TIMEOUT, 4096)).await;
    let took = from.elapsed();
    let (_, exit) = asked.map_err(|e| format!("Exec past its timeout: {e}"))?;
    if !exit.timed_out || exit.status == 0 {
        return Ok(Verdict::fail(format!("past its timeout, it ended {exit:?}")));
    }
    if took < TIMEOUT || took > TIMEOUT + LATE {
        return Ok(Verdict::fail(format!(
            "with a timeout of {TIMEOUT:?}, it ended after {took:?}"
        )));
    }
    let counted = said(s, &id, &probe::COUNT_STUBBORN).await?;
    Ok(Verdict::check(
        counted == "probe: counted 0",
        format!("timed out, it still runs: {counted}"),
    ))
}

/// What a command writes up to its limit is sent whole; past it, exactly the limit is sent and
/// the rest dropped, said to be truncated, and the command still ends of itself; and a limit past
/// the protocol's is refused or held to it.
pub async fn sends_no_more_than_its_limit(s: &Suite) -> Result<Verdict, Stopped> {
    if let Err(not) = declared(s) {
        return Ok(not);
    }
    let (id, _) = s.running(&probe::SERVE).await?;
    let exec = |command: [&'static str; 3], limit| {
        let d = s.driver();
        let id = id.clone();
        async move { d.exec(&id, &command, (QUICK, limit)).await.map_err(|e| format!("Exec: {e}")) }
    };
    let (output, exit) = exec(probe::write("512"), 1024).await?;
    if output.len() != 512 || exit.truncated || exit.status != 0 {
        return Ok(Verdict::fail(format!(
            "512 bytes within 1024 came as {}, {exit:?}",
            output.len()
        )));
    }
    let (output, exit) = exec(probe::write("200000"), 1024).await?;
    if output.len() != 1024 || !exit.truncated || exit.status != 0 || exit.timed_out {
        return Ok(Verdict::fail(format!(
            "200000 bytes past 1024 came as {}, {exit:?}",
            output.len()
        )));
    }
    let past = exec(probe::write("131072"), MOST + 1).await;
    Ok(match past {
        Err(_) => Verdict::Pass,
        Ok((output, exit)) => Verdict::check(
            u32::try_from(output.len()).is_ok_and(|n| n <= MOST) && exit.truncated,
            format!("a limit past 64 KiB was taken: {} bytes, {exit:?}", output.len()),
        ),
    })
}

/// A driver that does not declare `exec` refuses it.
pub async fn undeclared_is_refused(s: &Suite) -> Result<Verdict, Stopped> {
    if s.capabilities().exec {
        return Ok(Verdict::NotApplicable("it declares exec".into()));
    }
    let (id, _) = s.running(&probe::SERVE).await?;
    let asked = s.driver().exec(&id, &probe::WHOAMI, (QUICK, IDENTITY)).await;
    Ok(Verdict::check(asked.is_err(), "it declares no exec, and ran a command"))
}
