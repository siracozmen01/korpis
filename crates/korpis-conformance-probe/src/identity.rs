// SPDX-License-Identifier: Apache-2.0

//! Who the probe runs as, and what it says of the commands run beside it: so a command a driver
//! runs inside an object (`Exec`, §4.2 of docs/04-runtimes.md) is compared with the object's own
//! process, and its timeout and its output limit are seen from inside.

use super::say;
use std::io::Write as _;
use std::process::ExitCode;

/// The fields of `/proc/self/status` that say who a process is and what it may do.
const STATUS: [&str; 8] =
    ["Uid", "Gid", "Groups", "CapEff", "CapPrm", "CapBnd", "NoNewPrivs", "Seccomp"];
/// The namespaces a process is in, as `/proc/self/ns` names them.
const NAMESPACES: [&str; 8] = ["cgroup", "ipc", "mnt", "net", "pid", "time", "user", "uts"];

/// Who this process is, one line each: its ids, capabilities, and confinement; the namespaces it
/// is in; its security module's label; its cgroup; its limits; its working directory and its environment, sorted. A line it
/// cannot read says so, which compares equal only with another that cannot.
pub fn identity() -> String {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let mut lines: Vec<String> = STATUS
        .iter()
        .map(|field| {
            let prefix = format!("{field}:");
            let value = status.lines().find_map(|l| l.strip_prefix(prefix.as_str()));
            format!("{field}: {}", value.map_or("unread", str::trim))
        })
        .collect();
    for ns in NAMESPACES {
        let link = std::fs::read_link(format!("/proc/self/ns/{ns}"));
        let link = link.map_or_else(|e| format!("unread ({e})"), |l| l.display().to_string());
        lines.push(format!("ns {ns}: {link}"));
    }
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_else(|e| format!("unread {e}"));
    // The security module's label: an AppArmor profile or an SELinux context, where one is.
    lines
        .push(format!("label: {}", read("/proc/self/attr/current").trim_end_matches(['\0', '\n'])));
    // A guest's kernel may keep no cgroup of its own: its limits are the host's, outside it.
    let cgroups = read("/proc/self/cgroup");
    if cgroups.trim().is_empty() {
        lines.push("cgroup: none".to_owned());
    }
    lines.extend(cgroups.lines().map(|l| format!("cgroup: {l}")));
    lines.extend(read("/proc/self/limits").lines().map(|l| format!("limit: {}", l.trim_end())));
    let cwd =
        std::env::current_dir().map_or_else(|e| format!("unread {e}"), |d| d.display().to_string());
    lines.push(format!("cwd: {cwd}"));
    let mut env: Vec<String> = std::env::vars_os()
        .map(|(k, v)| format!("env: {}={}", k.to_string_lossy(), v.to_string_lossy()))
        .collect();
    env.sort();
    lines.extend(env);
    lines.join("\n")
}

/// Says who this process is.
pub fn whoami() -> ExitCode {
    say(&identity());
    ExitCode::SUCCESS
}

/// Says who the `serve` or `stubborn` beside it is, as it wrote it to `serving` once it took the
/// terminating signal; exits 1 if none has.
pub fn entry(serving: &str) -> ExitCode {
    match std::fs::read_to_string(serving) {
        Ok(written) => {
            say(&written);
            ExitCode::SUCCESS
        }
        Err(_) => ExitCode::FAILURE,
    }
}

/// Writes `bytes` bytes on its standard output, then exits 0.
pub fn write(bytes: &str) -> ExitCode {
    let Ok(mut left) = bytes.parse::<usize>() else { return ExitCode::from(2) };
    let block = [b'.'; 4096];
    let mut out = std::io::stdout().lock();
    while left > 0 {
        let n = left.min(block.len());
        if out.write_all(block.get(..n).unwrap_or_default()).is_err() {
            return ExitCode::FAILURE;
        }
        left -= n;
    }
    let _ = out.flush();
    ExitCode::SUCCESS
}

/// Says how many processes it sees whose command is the probe's `command`.
pub fn count(command: &str) -> ExitCode {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        say("probe: /proc cannot be read");
        return ExitCode::FAILURE;
    };
    let counted = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
        .filter(|line| line.split(|b| *b == 0).nth(1) == Some(command.as_bytes()))
        .count();
    say(&format!("probe: counted {counted}"));
    ExitCode::SUCCESS
}
