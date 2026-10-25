// SPDX-License-Identifier: Apache-2.0

//! The program the driver conformance suite runs as a driver's object (§8 of
//! docs/04-runtimes.md), built static so its image holds nothing else. What it does is its first
//! argument, and what it says goes to its standard output, one line each:
//!
//! - `serve`: runs until it is sent the terminating signal, then exits 0;
//! - `stubborn`: takes the terminating signal and goes on running, so only a kill ends it;
//! - `serving`: exits 0 if a `serve` or a `stubborn` beside it has taken the terminating signal,
//!   and 1 otherwise;
//! - `whoami`: says who it runs as: its ids, capabilities, namespaces, cgroup, limits, working
//!   directory, and environment;
//! - `entry`: says who the `serve` or `stubborn` beside it runs as, as `whoami` would have said it
//!   once it took the terminating signal, and exits 1 if none has;
//! - `write <bytes>`: writes that many bytes and exits 0;
//! - `count <command>`: says how many processes it sees running the probe's `command`;
//! - `exit <status>`: exits with that status at once;
//! - `alloc`: takes memory a mebibyte at a time, writing each and saying how much it holds, until
//!   the kernel ends it;
//! - `threads`: starts threads until one is refused, and says how many;
//! - `fill <dir>`: writes in `dir` until a write is refused, and says how much and why;
//! - `spin <threads>`: keeps that many threads busy until it is killed.
//! - `cpus`: says the logical CPUs it may run on, as the kernel lists them.

use std::io::Write as _;
use std::process::ExitCode;

mod identity;
mod limits;

/// The file `serve` and `stubborn` write once they take the terminating signal.
const SERVING: &str = "/tmp/probe-serving";

/// Says `line` on its standard output.
fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Runs until the terminating signal arrives; then, unless `stubborn`, exits 0.
fn until_terminated(stubborn: bool) -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_io().build();
    let Ok(runtime) = runtime else {
        say("probe: no runtime");
        return ExitCode::FAILURE;
    };
    runtime.block_on(async {
        let kind = tokio::signal::unix::SignalKind::terminate();
        let Ok(mut terminated) = tokio::signal::unix::signal(kind) else {
            say("probe: the terminating signal cannot be taken");
            return ExitCode::FAILURE;
        };
        // Said once the signal is taken, so a stop from now on reaches this handler; written
        // whole and renamed, so `serving` and `entry` never see half of it.
        let partial = format!("{SERVING}.partial");
        if std::fs::write(&partial, identity::identity()).is_ok() {
            let _ = std::fs::rename(&partial, SERVING);
        }
        say("probe: running");
        loop {
            terminated.recv().await;
            say("probe: terminated");
            if !stubborn {
                return ExitCode::SUCCESS;
            }
        }
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["serve"] => until_terminated(false),
        ["stubborn"] => until_terminated(true),
        ["exit", status] => ExitCode::from(status.parse::<u8>().unwrap_or(u8::MAX)),
        ["serving"] => {
            if std::path::Path::new(SERVING).exists() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        ["whoami"] => identity::whoami(),
        ["entry"] => identity::entry(SERVING),
        ["write", bytes] => identity::write(bytes),
        ["count", command] => identity::count(command),
        ["alloc"] => limits::alloc(),
        ["threads"] => limits::threads(),
        ["fill", dir] => limits::fill(dir),
        ["spin", threads] => limits::spin(threads),
        ["cpus"] => limits::cpus(),
        _ => {
            say("probe: serve, stubborn, serving, exit <status>, whoami, entry, write <bytes>, \
                 count <command>, alloc, threads, fill <dir>, spin <n>");
            ExitCode::from(2)
        }
    }
}
