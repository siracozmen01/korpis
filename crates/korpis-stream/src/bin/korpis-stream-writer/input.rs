// SPDX-License-Identifier: AGPL-3.0-or-later

//! Console input (§6 of docs/14-streams.md). For an interactive workload the driver hands the
//! writer two FIFOs: one it sends input on, framed, and the task's own stdin. The writer records
//! each input in the stream, redacted, naming who typed it and in which session, and then hands
//! the bytes to the workload. Being the shim's child it holds the task's stdin open for the
//! task's whole life, so a restart of the agent or the driver never ends the workload's input.
//!
//! Input never blocks the writer: it is handed to a thread of its own through a short queue, and
//! input that finds the queue full, because the workload is not reading its stdin, is dropped and
//! said to be dropped, rather than stopping the writer from draining the workload's output.
//!
//! A frame is a four-byte big-endian length and that many bytes of JSON: `{"by", "session",
//! "data"}` with the input in base64, or `{"by", "session", "resize": [cols, rows]}`. The driver
//! writes each frame whole and at most `PIPE_BUF` long, so frames never interleave.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::mpsc;

/// The longest frame read; the driver writes at most `PIPE_BUF` (4 KiB).
const MAX_FRAME: usize = 64 * 1024;
/// Inputs waiting for the workload to read them before more are dropped.
const QUEUED: usize = 64;

/// One input, as the driver framed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    /// Who typed it: the session token's subject.
    pub by: String,
    /// The data-path session it came on.
    pub session: String,
    /// What it is.
    pub what: What,
}

/// What an input is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    /// Bytes for the workload's stdin.
    Data(Vec<u8>),
    /// The terminal was resized to these columns and rows.
    Resize(u16, u16),
}

/// The input a frame's JSON says; none if it says nothing understood.
pub fn parse(frame: &[u8]) -> Option<Input> {
    let v: serde_json::Value = serde_json::from_slice(frame).ok()?;
    let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str).map(str::to_owned);
    let what = if let Some(data) = text("data") {
        What::Data(STANDARD.decode(data).ok()?)
    } else {
        let size = v.get("resize")?.as_array()?;
        let n = |i: usize| size.get(i)?.as_u64().and_then(|n| u16::try_from(n).ok());
        What::Resize(n(0)?, n(1)?)
    };
    Some(Input { by: text("by")?, session: text("session")?, what })
}

/// The record body for `input`, its data already redacted.
pub fn body(input: &Input, redacted: &[u8]) -> Vec<u8> {
    let v = match &input.what {
        What::Data(_) => serde_json::json!({
            "by": input.by,
            "session": input.session,
            "text": String::from_utf8_lossy(redacted),
        }),
        What::Resize(cols, rows) => serde_json::json!({
            "by": input.by,
            "session": input.session,
            "resize": [cols, rows],
        }),
    };
    v.to_string().into_bytes()
}

/// Reads frames from `from` and hands each input to `to`, until `to` is gone. The FIFO is opened
/// for reading and writing, so it never ends while the driver has it closed.
pub fn read_frames<M>(mut from: File, to: &mpsc::Sender<M>, wrap: impl Fn(Input) -> M) {
    let mut len = [0u8; 4];
    loop {
        if from.read_exact(&mut len).is_err() {
            return;
        }
        let n = usize::try_from(u32::from_be_bytes(len)).unwrap_or(usize::MAX);
        if n > MAX_FRAME {
            // A frame this long was not written by the driver: nothing after it can be trusted
            // to be aligned, so input ends here rather than being misread.
            return;
        }
        let mut frame = vec![0u8; n];
        if from.read_exact(&mut frame).is_err() {
            return;
        }
        if let Some(input) = parse(&frame)
            && to.send(wrap(input)).is_err()
        {
            return;
        }
    }
}

/// The workload's stdin, written by a thread of its own.
pub struct Stdin {
    queue: mpsc::SyncSender<Vec<u8>>,
}

impl Stdin {
    /// The stdin FIFO at `path`, opened for reading and writing so opening never waits for the
    /// shim and the shim never sees it end while the writer runs.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut fifo = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
        let (queue, rx) = mpsc::sync_channel::<Vec<u8>>(QUEUED);
        std::thread::spawn(move || {
            while let Ok(bytes) = rx.recv()
                && fifo.write_all(&bytes).is_ok()
            {}
        });
        Ok(Self { queue })
    }

    /// Hands `bytes` to the workload; false if they were dropped because it is not reading.
    pub fn send(&self, bytes: Vec<u8>) -> bool {
        self.queue.try_send(bytes).is_ok()
    }
}

/// Opens `path` for reading frames; for reading and writing, so it never ends.
pub fn open_frames(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new().read(true).write(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_say_who_typed_what_and_resizes() {
        let typed = br#"{"by":"user:alex","session":"s1","data":"c3RvcAo="}"#;
        let input = parse(typed).unwrap();
        assert_eq!(input.what, What::Data(b"stop\n".to_vec()));
        let body: serde_json::Value = serde_json::from_slice(&body(&input, b"stop\n")).unwrap();
        assert_eq!(
            (body["by"].as_str(), body["text"].as_str()),
            (Some("user:alex"), Some("stop\n"))
        );
        let resized = parse(br#"{"by":"u","session":"s","resize":[120,40]}"#).unwrap();
        assert_eq!(resized.what, What::Resize(120, 40));
        assert_eq!(parse(br#"{"by":"u","session":"s"}"#), None, "says nothing");
    }
}
