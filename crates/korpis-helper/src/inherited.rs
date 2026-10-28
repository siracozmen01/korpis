// SPDX-License-Identifier: AGPL-3.0-or-later

//! A socket a process inherited by number: the `splice` subcommand's, which the helper handed
//! rsync, and rsync its remote shell, without its close-on-exec flag (`tree`).
//!
//! This is one of the crate's unsafe modules, listed in the crate's header: taking ownership of a
//! descriptor known only by its number has no safe form, since nothing but the process's own
//! history says the number is open and owned by no one else.

#![expect(unsafe_code, reason = "adopting an inherited descriptor by number has no safe form")]

use std::os::fd::{FromRawFd as _, OwnedFd};

/// The socket open at `raw`, owned from now on, if `raw` is an open socket.
///
/// # Errors
///
/// `raw` is a standard stream, or not an open socket.
pub fn socket(raw: i32) -> std::io::Result<OwnedFd> {
    if raw <= 2 {
        return Err(std::io::Error::other("a standard stream is not the transfer's socket"));
    }
    // SAFETY: only `main`'s `splice` subcommand calls this, once, with the number the helper
    // put on its command line for the descriptor it left open across exec; nothing else in this
    // process opened, owns, or closes that number, and the check below refuses a number that
    // is not an open socket before anything is done with it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    match rustix::net::sockopt::socket_type(&fd) {
        Ok(_) => Ok(fd),
        Err(e) => {
            // Not ours to close if it is not the socket: forget it rather than close it.
            std::mem::forget(fd);
            Err(e.into())
        }
    }
}
