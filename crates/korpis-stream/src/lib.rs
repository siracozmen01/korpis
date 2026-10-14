// SPDX-License-Identifier: AGPL-3.0-or-later

//! Streams on disk (docs/14-streams.md): the one definition of a record and of the segment files
//! that hold them, shared by the process that writes a workload's stream and the agent that reads
//! it, so the two cannot disagree about what was written.
//!
//! A stream is a directory of segments, each a run of records by offset. The writer never blocks:
//! what exceeds the stream's rate, or a record's size, is dropped and said to be dropped, as a gap
//! record with an offset of its own (§3, §5); secret values are redacted before a record reaches
//! the file (§7). Synchronous and small on purpose: the writer runs once per workload, beside it.
//!
//! The crate's one unsafe module is the writer binary's `fds`, which adopts the descriptors the
//! containerd shim passes its logging binary; no safe form exists for that.

pub mod changes;
pub mod dir;
pub mod lines;
pub mod read;
pub mod record;
pub mod redact;
pub mod search;
pub mod segment;
pub mod writer;

pub use record::{Kind, Record, Source};
