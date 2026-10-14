// SPDX-License-Identifier: AGPL-3.0-or-later

//! The descriptors the shim hands its logging binary: the workload's stdout on 3, its stderr on
//! 4, and on 5 the write end of a pipe the shim waits on until it is closed, which says the
//! writer is ready.
//!
//! This is the crate's one unsafe module. Adopting an inherited descriptor as an owned one has no
//! safe form in the standard library. Each is adopted once, first thing in `main`, after checking
//! the process has it open, so nothing else in the process can own it.

#![expect(unsafe_code, reason = "adopting the descriptors the shim passed has no safe form")]

use std::fs::File;
use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};

/// What the shim passed.
pub struct Passed {
    /// The workload's standard output.
    pub stdout: File,
    /// Its standard error.
    pub stderr: File,
    /// Closed once the writer is ready.
    pub ready: OwnedFd,
}

fn adopt(fd: RawFd) -> Result<OwnedFd, String> {
    if std::fs::read_link(format!("/proc/self/fd/{fd}")).is_err() {
        return Err(format!("descriptor {fd} is not open: run me as a containerd logging binary"));
    }
    // SAFETY: the descriptor is open (checked just above), it was inherited from the shim, and
    // this is the only place in the process that turns it into an owner, called once from `main`
    // before anything else opens a file, so no other owner of it exists or will.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Adopts descriptors 3, 4, and 5.
pub fn take() -> Result<Passed, String> {
    Ok(Passed { stdout: File::from(adopt(3)?), stderr: File::from(adopt(4)?), ready: adopt(5)? })
}
