// SPDX-License-Identifier: Apache-2.0

//! What the probe does to meet a limit, saying as it goes how far it got, so whoever runs it
//! reads in its output where the kernel stopped it: memory taken a mebibyte at a time, threads
//! started until one is refused, a directory filled until a write is refused, processors kept
//! busy, and the processors it may run on.

use super::say;
use std::io::Write as _;
use std::process::ExitCode;

/// A mebibyte.
const MIB: usize = 1 << 20;

/// Takes memory a mebibyte at a time, each page written so the kernel charges it, and never
/// lets any go, saying how much it holds after each, until it is killed for it.
pub fn alloc() -> ExitCode {
    let mut held: Vec<Vec<u8>> = Vec::new();
    loop {
        held.push(vec![1; MIB]);
        std::hint::black_box(&held);
        say(&format!("probe: holds {} MiB", held.len()));
    }
}

/// Starts threads that wait forever until one is refused, then says how many run beside this
/// one and exits 0.
pub fn threads() -> ExitCode {
    let mut started = 0u64;
    while std::thread::Builder::new().stack_size(64 << 10).spawn(std::thread::park).is_ok() {
        started += 1;
    }
    say(&format!("probe: started {started} threads"));
    ExitCode::SUCCESS
}

/// Writes a file in `dir` a mebibyte at a time until a write is refused, then says how many it
/// wrote and why it stopped, and exits 0.
pub fn fill(dir: &str) -> ExitCode {
    let path = std::path::Path::new(dir).join("probe-fill");
    let Ok(mut file) = std::fs::File::create(&path) else {
        say(&format!("probe: {} cannot be written", path.display()));
        return ExitCode::FAILURE;
    };
    let chunk = vec![1u8; MIB];
    let mut written = 0u64;
    let why = loop {
        match file.write_all(&chunk).and_then(|()| file.sync_data()) {
            Ok(()) => written += 1,
            Err(e) => break e,
        }
    };
    say(&format!("probe: wrote {written} MiB, then {why}"));
    let _ = std::fs::remove_file(&path);
    ExitCode::SUCCESS
}

/// Keeps `threads` threads busy forever.
pub fn spin(threads: &str) -> ExitCode {
    let n: usize = threads.parse().unwrap_or(1);
    for _ in 1..n {
        std::thread::spawn(busy);
    }
    say(&format!("probe: spinning on {n} threads"));
    busy()
}

/// Spins until killed.
fn busy() -> ! {
    let mut x = 0u64;
    loop {
        x = std::hint::black_box(x.wrapping_add(1));
    }
}

/// Says the logical CPUs it may run on, as `/proc/self/status` lists them, and exits 0; or
/// says it could not read them and exits 1.
pub fn cpus() -> ExitCode {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let Some(list) = status.lines().find_map(|l| l.strip_prefix("Cpus_allowed_list:")) else {
        say("probe: cpus unread");
        return ExitCode::FAILURE;
    };
    say(&format!("probe: cpus {}", list.trim()));
    ExitCode::SUCCESS
}
