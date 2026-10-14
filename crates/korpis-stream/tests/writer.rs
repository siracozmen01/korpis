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
    let (logged, _) = changes::read_from(&dir, 0).unwrap();
    assert!(
        matches!(logged.as_slice(), [Change::Sealed(s)] if s.end_offset == 4),
        "the last segment is sealed and recorded: {logged:?}"
    );
}
