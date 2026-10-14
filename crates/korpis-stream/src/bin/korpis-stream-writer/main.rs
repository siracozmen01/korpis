// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-stream-writer`: one workload's stream writer (§4 of docs/14-streams.md). The oci
//! driver names it as the task's logging binary, so containerd's shim starts it with the task and
//! hands it the workload's stdout and stderr. It drains both for as long as the task lives, so the
//! workload never waits on its own output (§5), and writes them into the stream's directory,
//! redacted (§7), limited, and cut into segments, each seal and prune recorded for the agent to
//! journal. Being the shim's child, it outlives a restart of the agent or the driver.
//!
//! When the task is deleted the shim closes the workload's pipes and sends the writer SIGTERM,
//! killing it a few seconds later if it is still there. The writer takes SIGTERM as the end: it
//! drains what is still in the pipes for at most [`LAST_DRAIN`], seals, and exits, so the last
//! segment is sealed and recorded rather than left open for a writer that never comes.

mod args;
mod fds;
mod input;
mod sink;

use korpis_stream::dir::StreamDir;
use korpis_stream::redact::Redactor;
use sink::Sink;
use std::io::Read as _;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often the writer ticks: seals by age, flushes waiting lines, makes writes durable.
const TICK: Duration = Duration::from_millis(100);

/// How long the writer drains after SIGTERM before it seals, well inside the twelve seconds
/// containerd's shim waits before it kills its logging binary.
const LAST_DRAIN: Duration = Duration::from_secs(3);

enum Message {
    Bytes(usize, Vec<u8>),
    Ended,
    Input(input::Input),
}

fn now() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
}

/// Reads `from` until it ends, sending what it reads as `source`.
fn drain(mut from: std::fs::File, source: usize, to: &mpsc::Sender<Message>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to
                    .send(Message::Bytes(source, buf.get(..n).unwrap_or_default().to_vec()))
                    .is_err()
                {
                    return;
                }
            }
        }
    }
    let _ = to.send(Message::Ended);
}

fn redactor(path: Option<&std::path::Path>) -> Result<Redactor, String> {
    let Some(path) = path else { return Ok(Redactor::default()) };
    let json = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Redactor::from_json(&json)?.0)
}

#[expect(clippy::print_stderr, reason = "the writer's only voice is its standard error")]
fn fail(e: &str) -> ExitCode {
    eprintln!("korpis-stream-writer: {e}");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let passed = match fds::take() {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let (args, redactor) = match args::parse(std::env::args().skip(1))
        .and_then(|a| redactor(a.secrets.as_deref()).map(|r| (a, r)))
    {
        Ok(found) => found,
        Err(e) => return fail(&e),
    };
    if let Err(e) = std::fs::create_dir_all(&args.dir) {
        return fail(&format!("{}: {e}", args.dir.display()));
    }
    let terminated = Arc::new(AtomicBool::new(false));
    if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGTERM, terminated.clone()) {
        return fail(&format!("SIGTERM: {e}"));
    }
    let mut sink = Sink::new(StreamDir::new(&args.dir), args.limits, redactor, now());
    let (tx, rx) = mpsc::channel();
    for (file, source) in [(passed.stdout, 0), (passed.stderr, 1)] {
        let tx = tx.clone();
        // A reader still blocked when the writer is done ends with the process.
        std::thread::spawn(move || drain(file, source, &tx));
    }
    let stdin = match &args.input {
        Some((frames, stdin)) => match console(frames, stdin, &tx) {
            Ok(s) => Some(s),
            Err(e) => return fail(&e),
        },
        None => None,
    };
    drop(tx);
    // Ready: the shim starts the task once this is closed.
    drop(passed.ready);
    run(&mut sink, &rx, (&terminated, stdin.as_ref()));
    ExitCode::SUCCESS
}

/// Opens an interactive workload's input: the frames the driver sends, read on a thread of their
/// own, and the task's stdin.
fn console(
    frames: &std::path::Path,
    stdin: &std::path::Path,
    tx: &mpsc::Sender<Message>,
) -> Result<input::Stdin, String> {
    let from = input::open_frames(frames).map_err(|e| format!("{}: {e}", frames.display()))?;
    let stdin = input::Stdin::open(stdin).map_err(|e| format!("{}: {e}", stdin.display()))?;
    let tx = tx.clone();
    std::thread::spawn(move || input::read_frames(from, &tx, Message::Input));
    Ok(stdin)
}

/// Records `i` and hands its data to the workload.
fn take_input(sink: &mut Sink, stdin: Option<&input::Stdin>, i: &input::Input, now: i64) {
    sink.input(i, now);
    if let (input::What::Data(data), Some(stdin)) = (&i.what, stdin)
        && !stdin.send(data.clone())
    {
        sink.input_dropped(u64::try_from(data.len()).unwrap_or(u64::MAX), now);
    }
}

/// Writes what arrives until both sources end, or for [`LAST_DRAIN`] after `terminated` is set.
fn run(
    sink: &mut Sink,
    rx: &mpsc::Receiver<Message>,
    (terminated, stdin): (&AtomicBool, Option<&input::Stdin>),
) {
    let mut open = 2;
    let mut ticked = now();
    let mut deadline: Option<Instant> = None;
    while open > 0 && deadline.is_none_or(|d| Instant::now() < d) {
        if deadline.is_none() && terminated.load(Ordering::Relaxed) {
            deadline = Some(Instant::now() + LAST_DRAIN);
        }
        match rx.recv_timeout(TICK) {
            Ok(Message::Bytes(source, bytes)) => sink.push(source, &bytes, now()),
            Ok(Message::Ended) => open -= 1,
            Ok(Message::Input(i)) => take_input(sink, stdin, &i, now()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let at = now();
        if at - ticked >= i64::try_from(TICK.as_nanos()).unwrap_or(i64::MAX) {
            sink.tick(at);
            ticked = at;
        }
    }
    sink.finish(now());
}
