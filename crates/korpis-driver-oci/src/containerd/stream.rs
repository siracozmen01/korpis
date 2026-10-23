// SPDX-License-Identifier: AGPL-3.0-or-later

//! A container's console stream (§4 of docs/14-streams.md). The driver names the stream writer as
//! the task's logging binary in containerd's `binary://` form, so the shim starts it with every
//! task and hands it the workload's stdout and stderr; what it is told comes from the container's
//! labels, kept with the container so a task started after a driver restart is told the same.
//! The secret values it redacts are not labels, which anyone reading containerd's records could
//! see: they are written to a file in the driver's state directory, readable by the driver's user
//! alone, and the writer is given its path.
//!
//! An interactive console takes input (§6 of docs/14-streams.md). The driver makes two FIFOs in
//! its state directory: the task's stdin, which the writer holds open for the task's whole life
//! and feeds, and the one the driver sends input on, framed, so the writer records who typed what
//! before handing it to the workload. The driver opens the second for each input without waiting:
//! a writer that is not running, or not reading, refuses the input rather than stalling the call.

use crate::error::{DriverError, DriverResult};
use crate::validate::Valid;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DIR: &str = "korpis.stream.dir";
const RATE: &str = "korpis.stream.rate";
const BURST: &str = "korpis.stream.burst";
const RETENTION_BYTES: &str = "korpis.stream.retention_bytes";
const RETENTION_SECONDS: &str = "korpis.stream.retention_seconds";
const INPUT: &str = "korpis.stream.input";
/// Where the writer is, and where the driver keeps the files it hands it.
#[derive(Debug, Clone)]
pub struct Streams {
    /// The `korpis-stream-writer` binary.
    pub writer: PathBuf,
    /// The driver's own state, readable by it alone.
    pub state: PathBuf,
}

/// The labels that tell a task's writer what to do, if the container has a stream.
pub(crate) fn labels(valid: &Valid) -> BTreeMap<String, String> {
    let Some(s) = valid.spec().stream.as_option() else { return BTreeMap::new() };
    BTreeMap::from([
        (DIR.to_owned(), s.dir.clone()),
        (RATE.to_owned(), s.rate_bytes_per_sec.to_string()),
        (BURST.to_owned(), s.burst_bytes.to_string()),
        (RETENTION_BYTES.to_owned(), s.retention_bytes.to_string()),
        (RETENTION_SECONDS.to_owned(), s.retention_seconds.to_string()),
        (INPUT.to_owned(), s.input.to_string()),
    ])
}

/// Whether a container labelled `labels` has a console that takes input.
pub(crate) fn takes_input(labels: &BTreeMap<String, String>) -> bool {
    labels.get(INPUT).is_some_and(|v| v == "true")
}

/// The frames that carry `data` from `by` in `session`, each small enough to be written whole.
pub(crate) fn frames(by: &str, session: &str, data: &[u8]) -> DriverResult<Vec<Vec<u8>>> {
    korpis_driver_kit::console::frames(by, session, data).map_err(DriverError::Rejected)
}

impl Streams {
    /// The secrets file of container `id`.
    fn secrets_path(&self, id: &str) -> PathBuf {
        self.path(id, "json")
    }

    fn path(&self, id: &str, extension: &str) -> PathBuf {
        self.state.join("streams").join(format!("{id}.{extension}"))
    }

    /// The task's stdin, for a container whose console takes input.
    pub(crate) fn stdin(&self, id: &str, labels: &BTreeMap<String, String>) -> Option<String> {
        takes_input(labels).then(|| self.path(id, "stdin").to_string_lossy().into_owned())
    }

    /// Makes the FIFOs an interactive console of container `id` needs.
    pub(crate) fn make_input(&self, valid: &Valid, id: &str) -> DriverResult<()> {
        if !valid.spec().stream.as_option().is_some_and(|s| s.input) {
            return Ok(());
        }
        for extension in ["stdin", "input"] {
            let path = self.path(id, extension);
            korpis_driver_kit::console::make_fifo(&path)
                .map_err(|e| DriverError::Failed(format!("{}: {e}", path.display())))?;
        }
        Ok(())
    }

    /// Writes `frame` to container `id`'s writer, without waiting for it.
    pub(crate) fn send(&self, id: &str, frame: &[u8]) -> DriverResult<()> {
        korpis_driver_kit::console::send(&self.path(id, "input"), frame)
            .map_err(DriverError::Failed)
    }

    /// Writes the secret values `valid`'s stream redacts, for container `id`, readable by the
    /// driver's user alone; replaced if a create is asked again.
    pub(crate) fn write_secrets(&self, valid: &Valid, id: &str) -> DriverResult<()> {
        let Some(s) = valid.spec().stream.as_option() else { return Ok(()) };
        let path = self.secrets_path(id);
        let io = |e: std::io::Error| DriverError::Failed(format!("{}: {e}", path.display()));
        let dir = path.parent().ok_or_else(|| DriverError::Failed("no state directory".into()))?;
        std::fs::create_dir_all(dir).map_err(io)?;
        korpis_driver_kit::console::write_secrets(&path, s).map_err(io)
    }

    /// Forgets container `id`'s secrets file and input FIFOs.
    pub(crate) fn forget(&self, id: &str) -> DriverResult<()> {
        for extension in ["json", "stdin", "input"] {
            match std::fs::remove_file(self.path(id, extension)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    let why = format!("forgetting the stream files of {id}: {e}");
                    return Err(DriverError::Failed(why));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The `binary://` URI a task of container `id`, labelled `labels`, writes its stdio to;
    /// none if it has no stream.
    pub(crate) fn stdio(&self, id: &str, labels: &BTreeMap<String, String>) -> Option<String> {
        let dir = labels.get(DIR).filter(|d| !d.is_empty())?;
        let mut query = vec![format!("--dir={}", encode(dir))];
        for (flag, label) in [
            ("--rate", RATE),
            ("--burst", BURST),
            ("--retention-bytes", RETENTION_BYTES),
            ("--retention-seconds", RETENTION_SECONDS),
        ] {
            if let Some(v) = labels.get(label).filter(|v| v.as_str() != "0") {
                query.push(format!("{flag}={}", encode(v)));
            }
        }
        let secrets = self.secrets_path(id);
        if secrets.exists() {
            query.push(format!("--secrets={}", encode(&secrets.to_string_lossy())));
        }
        if takes_input(labels) {
            for (flag, extension) in [("--input", "input"), ("--stdin", "stdin")] {
                let path = self.path(id, extension);
                query.push(format!("{flag}={}", encode(&path.to_string_lossy())));
            }
        }
        Some(format!("binary://{}?{}", path_part(&self.writer), query.join("&")))
    }
}

fn path_part(p: &Path) -> String {
    p.to_string_lossy().split('/').map(encode).collect::<Vec<_>>().join("/")
}

/// `s` percent-encoded but for the characters a URI never needs to encode.
fn encode(s: &str) -> String {
    s.bytes().fold(String::with_capacity(s.len()), |mut out, b| {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push('%');
            for nibble in [b >> 4, b & 0xf] {
                out.push(char::from(HEX.get(usize::from(nibble)).copied().unwrap_or(b'0')));
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uri_names_the_writer_and_only_the_limits_set() {
        let streams = Streams {
            writer: "/usr/libexec/korpis/korpis-stream-writer".into(),
            state: "/nonexistent".into(),
        };
        let labels = BTreeMap::from([
            (DIR.to_owned(), "/var/lib/korpis/volumes/wl_1.stream_console".to_owned()),
            (RATE.to_owned(), "262144".to_owned()),
            (RETENTION_SECONDS.to_owned(), "0".to_owned()),
        ]);
        assert_eq!(
            streams.stdio("c1", &labels).unwrap(),
            "binary:///usr/libexec/korpis/korpis-stream-writer?\
             --dir=%2Fvar%2Flib%2Fkorpis%2Fvolumes%2Fwl_1.stream_console&--rate=262144"
        );
        assert_eq!(streams.stdio("c1", &BTreeMap::new()), None, "no stream, no writer");
    }
}
