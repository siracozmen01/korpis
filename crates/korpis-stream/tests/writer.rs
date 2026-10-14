// SPDX-License-Identifier: AGPL-3.0-or-later

//! The writer binary as a shim starts it: the workload's stdout and stderr on descriptors 3 and 4,
//! and a pipe on 5 it closes when ready. Named pipes stand in for the shim's, opened by a shell in
//! the order the shim passes them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_stream::changes::{self, Change};
use korpis_stream::dir::StreamDir;
use korpis_stream::read::read;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::Command;

fn fifo(path: &Path) {
    assert!(Command::new("mkfifo").arg(path).status().unwrap().success());
}

#[test]
fn the_writer_drains_both_streams_redacts_and_seals_when_the_workload_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, err, ready) =
        (tmp.path().join("out"), tmp.path().join("err"), tmp.path().join("ready"));
    for p in [&out, &err, &ready] {
        fifo(p);
    }
    let secrets = tmp.path().join("secrets.json");
    std::fs::write(&secrets, br#"[{"name":"rcon_password","value":"hunter22"}]"#).unwrap();
    let dir = tmp.path().join("stream");
    let mut writer = Command::new("sh")
        .arg("-c")
        .arg(r#"exec "$0" --secrets "$1" --dir "$2" --rate 1048576 3<"$3" 4<"$4" 5>"$5""#)
        .arg(env!("CARGO_BIN_EXE_korpis-stream-writer"))
        .args([&secrets, &dir, &out, &err, &ready])
        .spawn()
        .unwrap();
    let mut stdout = std::fs::OpenOptions::new().write(true).open(&out).unwrap();
    let mut stderr = std::fs::OpenOptions::new().write(true).open(&err).unwrap();
    let mut signal = std::fs::File::open(&ready).unwrap();
    let mut nothing = Vec::new();
    signal.read_to_end(&mut nothing).unwrap();
    assert!(nothing.is_empty(), "ready is said by closing, not by writing");

    stdout.write_all(b"Starting server\nRCON password hunter22\nprompt> ").unwrap();
    stderr.write_all(b"a warning\n").unwrap();
    drop((stdout, stderr));
    assert!(writer.wait().unwrap().success());

    let batch = read(&StreamDir::new(&dir), 0, 100).unwrap();
    let mut bodies: Vec<String> =
        batch.records.iter().map(|r| String::from_utf8_lossy(&r.body).into_owned()).collect();
    bodies.sort();
    assert_eq!(
        bodies,
        [
            "RCON password ████████ (redacted: rcon_password)",
            "Starting server",
            "a warning",
            "prompt> ",
        ]
    );
    let logged = changes::read_from(&dir, 0).unwrap();
    assert!(
        matches!(logged.as_slice(), [(Change::Sealed(s), _)] if s.end_offset == 4),
        "the last segment is sealed and recorded: {logged:?}"
    );
}

#[test]
fn sigterm_seals_what_was_written_though_the_pipes_stay_open() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, err, ready) =
        (tmp.path().join("out"), tmp.path().join("err"), tmp.path().join("ready"));
    for p in [&out, &err, &ready] {
        fifo(p);
    }
    let dir = tmp.path().join("stream");
    let mut writer = Command::new("sh")
        .arg("-c")
        .arg(r#"exec "$0" --dir "$1" 3<"$2" 4<"$3" 5>"$4""#)
        .arg(env!("CARGO_BIN_EXE_korpis-stream-writer"))
        .args([&dir, &out, &err, &ready])
        .spawn()
        .unwrap();
    let mut stdout = std::fs::OpenOptions::new().write(true).open(&out).unwrap();
    let _stderr = std::fs::OpenOptions::new().write(true).open(&err).unwrap();
    std::fs::File::open(&ready).unwrap().read_to_end(&mut Vec::new()).unwrap();
    stdout.write_all(b"the last words\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let started = std::time::Instant::now();
    let pid = writer.id().to_string();
    assert!(Command::new("kill").args(["-TERM", &pid]).status().unwrap().success());
    assert!(writer.wait().unwrap().success(), "SIGTERM is an end, not a death");
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "inside the shim's patience");
    let logged = changes::read_from(&dir, 0).unwrap();
    assert!(
        matches!(logged.as_slice(), [(Change::Sealed(s), _)] if s.end_offset == 1),
        "the one record is sealed and recorded: {logged:?}"
    );
}

fn frame(json: &str) -> Vec<u8> {
    let mut f = u32::try_from(json.len()).unwrap().to_be_bytes().to_vec();
    f.extend_from_slice(json.as_bytes());
    f
}

#[test]
fn input_is_recorded_redacted_with_who_typed_it_and_then_reaches_the_workload() {
    let tmp = tempfile::tempdir().unwrap();
    let names = ["out", "err", "ready", "input", "stdin"];
    let [out, err, ready, input, stdin] = names.map(|n| tmp.path().join(n));
    for p in [&out, &err, &ready, &input, &stdin] {
        fifo(p);
    }
    let secrets = tmp.path().join("secrets.json");
    std::fs::write(&secrets, br#"[{"name":"rcon_password","value":"hunter22"}]"#).unwrap();
    let dir = tmp.path().join("stream");
    let mut writer = Command::new("sh")
        .arg("-c")
        .arg(
            r#"exec "$0" --dir "$1" --secrets "$2" --input "$3" --stdin "$4" 3<"$5" 4<"$6" 5>"$7""#,
        )
        .arg(env!("CARGO_BIN_EXE_korpis-stream-writer"))
        .args([&dir, &secrets, &input, &stdin, &out, &err, &ready])
        .spawn()
        .unwrap();
    let stdout = std::fs::OpenOptions::new().write(true).open(&out).unwrap();
    let stderr = std::fs::OpenOptions::new().write(true).open(&err).unwrap();
    std::fs::File::open(&ready).unwrap().read_to_end(&mut Vec::new()).unwrap();
    // The workload's side of its stdin, as the shim would read it.
    let mut workload = std::fs::File::open(&stdin).unwrap();
    let mut driver = std::fs::OpenOptions::new().write(true).open(&input).unwrap();
    // "login hunter22\n", in base64.
    driver
        .write_all(&frame(r#"{"by":"user:alex","session":"s1","data":"bG9naW4gaHVudGVyMjIK"}"#))
        .unwrap();
    driver.write_all(&frame(r#"{"by":"user:alex","session":"s1","resize":[120,40]}"#)).unwrap();
    let mut got = [0u8; 15];
    workload.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"login hunter22\n", "the workload gets what was typed, unredacted");
    drop((stdout, stderr));
    assert!(writer.wait().unwrap().success());

    let records = read(&StreamDir::new(&dir), 0, 10).unwrap().records;
    assert_eq!(records.len(), 2, "{records:?}");
    let typed: serde_json::Value = serde_json::from_slice(&records[0].body).unwrap();
    assert_eq!(records[0].source, korpis_stream::Source::Input);
    assert_eq!(typed["by"], "user:alex");
    assert_eq!(typed["text"], "login ████████ (redacted: rcon_password)\n", "the stream does not");
    assert_eq!(records[1].kind, korpis_stream::Kind::Control);
}
