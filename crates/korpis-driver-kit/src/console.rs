// SPDX-License-Identifier: AGPL-3.0-or-later

//! A console's files on the driver's side (§4 and §6 of docs/14-streams.md), the same whichever
//! driver runs the workload, since each hands its console to `korpis-stream-writer`: the stream
//! directory a spec may name, the writer's arguments, the secret values it redacts, kept in a
//! file only the driver's user reads, and an interactive console's input. Input reaches the
//! writer through a FIFO the driver writes framed input to and the writer reads, so the writer
//! records who typed what before it hands the bytes to the workload; the driver opens the FIFO
//! for each input without waiting, so a writer that is not running, or not reading, refuses the
//! input rather than stalling the call.

use base64::Engine as _;
use korpis_core::workload::stream::StreamKind;
use korpis_proto::korpis::driver::v1 as driver;
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Component, Path};

/// The most a frame may be, so the FIFO takes it whole and two never interleave (`PIPE_BUF`).
const PIPE_BUF: usize = 4096;
/// Room left in a frame for who sent it and the JSON around the data.
const ENVELOPE: usize = 1024;

/// Whether `spec`'s stream, if it has one, is written to its own stream volume beneath
/// `volume_root`, or, for an object that executes one of its runs, to that run's: a run writes
/// its own console, and never another run's or the workload's.
#[must_use]
pub fn own_dir(spec: &driver::PreparedSpec, volume_root: &Path) -> bool {
    let Some(s) = spec.stream.as_option() else { return true };
    let path = Path::new(&s.dir);
    let run = spec.labels.as_option().map_or("", |l| l.run.as_str());
    let volume = match run {
        "" => StreamKind::Console.volume(&spec.workload),
        run => StreamKind::Console.run_volume(&spec.workload, run),
    };
    let clean = path.is_absolute()
        && path.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    clean
        && path.file_name().is_some_and(|n| n == volume.as_str())
        && path.strip_prefix(volume_root).is_ok()
}

/// The writer's arguments for `stream`, as flag and value, but for the files the driver keeps:
/// the stream's directory, and each limit that is set.
#[must_use]
pub fn writer_args(stream: &driver::StreamCapture) -> Vec<(&'static str, String)> {
    let mut args = vec![("--dir", stream.dir.clone())];
    for (flag, value) in [
        ("--rate", stream.rate_bytes_per_sec),
        ("--burst", stream.burst_bytes),
        ("--retention-bytes", stream.retention_bytes),
        ("--retention-seconds", stream.retention_seconds),
    ] {
        if value != 0 {
            args.push((flag, value.to_string()));
        }
    }
    args
}

/// The frames that carry `data` from `by` in `session`, each small enough to be written whole;
/// refused, saying why, if the names leave no room for data.
pub fn frames(by: &str, session: &str, data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    if by.len() + session.len() > ENVELOPE / 2 {
        return Err("the sender and session names are too long".into());
    }
    let chunk = (PIPE_BUF - ENVELOPE) / 4 * 3;
    let frames = data.chunks(chunk).map(|part| {
        let data = base64::engine::general_purpose::STANDARD.encode(part);
        frame(&serde_json::json!({ "by": by, "session": session, "data": data }))
    });
    Ok(frames.collect())
}

/// The frame that says the terminal was resized.
#[must_use]
pub fn resize_frame(by: &str, session: &str, cols: u16, rows: u16) -> Vec<u8> {
    frame(&serde_json::json!({ "by": by, "session": session, "resize": [cols, rows] }))
}

fn frame(json: &serde_json::Value) -> Vec<u8> {
    let body = json.to_string().into_bytes();
    let mut out = u32::try_from(body.len()).unwrap_or(u32::MAX).to_be_bytes().to_vec();
    out.extend(body);
    out
}

/// Makes a FIFO at `path`, readable and writable by the driver's user alone; one already there
/// is kept.
pub fn make_fifo(path: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, FileType, Mode, mknodat};
    match mknodat(CWD, path, FileType::Fifo, Mode::RUSR | Mode::WUSR, 0) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Writes `frame` to the writer reading the FIFO at `path`, without waiting for it; the error
/// says what a caller can be told.
pub fn send(path: &Path, frame: &[u8]) -> Result<(), String> {
    use rustix::fs::{Mode, OFlags, open};
    let flags = OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = open(path, flags, Mode::empty()).map_err(|e| match e {
        rustix::io::Errno::NXIO | rustix::io::Errno::NOENT => {
            "the console's writer is not running".to_owned()
        }
        e => format!("{}: {e}", path.display()),
    })?;
    std::fs::File::from(fd).write_all(frame).map_err(|e| match e.kind() {
        io::ErrorKind::WouldBlock => {
            "the console's input is full: the workload is not reading".into()
        }
        _ => format!("{}: {e}", path.display()),
    })
}

/// Writes the secret values `stream` redacts at `path`, readable by the driver's user alone,
/// whole or not at all; replaced if it is written again.
pub fn write_secrets(path: &Path, stream: &driver::StreamCapture) -> io::Result<()> {
    let secrets: Vec<_> = stream
        .secrets
        .iter()
        .map(|x| serde_json::json!({ "name": x.name, "value": x.value }))
        .collect();
    let json = serde_json::to_vec(&secrets).map_err(io::Error::other)?;
    let beside = path.with_extension("new");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&beside)?;
    f.write_all(&json)?;
    f.sync_all()?;
    std::fs::rename(&beside, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(dir: &str, run: &str) -> driver::PreparedSpec {
        driver::PreparedSpec {
            workload: "wl_1".into(),
            stream: driver::StreamCapture { dir: dir.into(), ..Default::default() }.into(),
            labels: driver::RuntimeLabels { run: run.into(), ..Default::default() }.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_stream_is_written_to_its_own_volume_alone() {
        let root = Path::new("/var/lib/korpis/volumes");
        assert!(own_dir(&spec("/var/lib/korpis/volumes/wl_1.stream_console", ""), root));
        assert!(own_dir(&spec("/var/lib/korpis/volumes/wl_1.stream_console.run_7", "run_7"), root));
        for (dir, run) in [
            ("/var/lib/korpis/volumes/wl_2.stream_console", ""),
            ("/var/lib/korpis/volumes/wl_1.stream_console", "run_7"),
            ("/var/lib/korpis/volumes/../volumes/wl_1.stream_console", ""),
            ("/elsewhere/wl_1.stream_console", ""),
        ] {
            assert!(!own_dir(&spec(dir, run), root), "{dir} for {run:?}");
        }
        assert!(own_dir(&driver::PreparedSpec::default(), root), "no stream, nothing to check");
    }

    #[test]
    fn the_writer_is_told_only_the_limits_set_and_input_is_framed_whole() {
        let stream = driver::StreamCapture {
            dir: "/v/wl_1.stream_console".into(),
            rate_bytes_per_sec: 262_144,
            ..Default::default()
        };
        let args = writer_args(&stream);
        assert_eq!(args, [("--dir", "/v/wl_1.stream_console".into()), ("--rate", "262144".into())]);
        let framed = frames("user:alex", "s1", &[b'x'; 10_000]).unwrap();
        assert!(framed.len() > 1 && framed.iter().all(|f| f.len() <= PIPE_BUF));
        assert!(frames(&"u".repeat(600), "s1", b"x").is_err());
    }
}
