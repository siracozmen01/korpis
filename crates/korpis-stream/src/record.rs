// SPDX-License-Identifier: AGPL-3.0-or-later

//! A record (§2 of docs/14-streams.md): an offset, the node clock at write, where it came from,
//! what kind it is, and its bytes. Offsets are the identity; times are informational.

/// Where a record came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The workload's standard output.
    Stdout,
    /// Its standard error.
    Stderr,
    /// A file it writes, by path inside the workload.
    File(String),
    /// Korpis itself: gap records, and what the platform says into the stream.
    Korpis,
    /// Console input, as typed.
    Input,
}

/// What a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Bytes as written.
    Text,
    /// One JSON value the workload wrote whole.
    Structured,
    /// Records or bytes lost, how many, over how long, and why (§3).
    Gap,
    /// A terminal event: resize, signal, end of input.
    Control,
}

/// One record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Its position in the stream: monotonic, from 0, never reused once read.
    pub offset: u64,
    /// The node clock when it was written, in nanoseconds since the Unix epoch.
    pub time_nanos: i64,
    /// Where it came from.
    pub source: Source,
    /// What it is.
    pub kind: Kind,
    /// Its bytes.
    pub body: Vec<u8>,
}

impl Source {
    /// The tag a frame stores, and the path for a file.
    pub(crate) fn tag(&self) -> (u8, &[u8]) {
        match self {
            Self::Stdout => (0, b""),
            Self::Stderr => (1, b""),
            Self::File(path) => (2, path.as_bytes()),
            Self::Korpis => (3, b""),
            Self::Input => (4, b""),
        }
    }

    /// The source a frame's tag and path name.
    pub(crate) fn from_tag(tag: u8, path: &[u8]) -> Option<Self> {
        Some(match tag {
            0 => Self::Stdout,
            1 => Self::Stderr,
            2 => Self::File(String::from_utf8(path.to_vec()).ok()?),
            3 => Self::Korpis,
            4 => Self::Input,
            _ => return None,
        })
    }

    /// Its name, as §2 writes it: `stdout`, `stderr`, `file:<path>`, `korpis`, `input`.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Stdout => "stdout".to_owned(),
            Self::Stderr => "stderr".to_owned(),
            Self::File(path) => format!("file:{path}"),
            Self::Korpis => "korpis".to_owned(),
            Self::Input => "input".to_owned(),
        }
    }
}

impl Kind {
    /// The tag a frame stores.
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Text => 0,
            Self::Structured => 1,
            Self::Gap => 2,
            Self::Control => 3,
        }
    }

    /// The kind a frame's tag names.
    pub(crate) const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Text),
            1 => Some(Self::Structured),
            2 => Some(Self::Gap),
            3 => Some(Self::Control),
            _ => None,
        }
    }

    /// Its name: `text`, `structured`, `gap`, `control`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Structured => "structured",
            Self::Gap => "gap",
            Self::Control => "control",
        }
    }
}

/// What a gap record says: how much was lost, over how long, and why (§3 of
/// docs/14-streams.md). Its body is this, as JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    /// Records lost whole.
    pub records: u64,
    /// Bytes lost, the cut part of an overlong record included.
    pub bytes: u64,
    /// From the first loss to the last, in milliseconds.
    pub millis: u64,
    /// Why, for a person.
    pub reason: String,
}

impl Gap {
    /// The record body.
    #[must_use]
    pub fn body(&self) -> Vec<u8> {
        serde_json::json!({
            "records": self.records,
            "bytes": self.bytes,
            "millis": self.millis,
            "reason": self.reason,
        })
        .to_string()
        .into_bytes()
    }

    /// What a gap record's body says; none if it is not a gap's.
    #[must_use]
    pub fn parse(body: &[u8]) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_slice(body).ok()?;
        let n = |k: &str| v.get(k).and_then(serde_json::Value::as_u64);
        Some(Self {
            records: n("records")?,
            bytes: n("bytes")?,
            millis: n("millis")?,
            reason: v.get("reason")?.as_str()?.to_owned(),
        })
    }
}
