// SPDX-License-Identifier: AGPL-3.0-or-later

//! The local terminal, for `korpis console`: put into raw mode while attached, so every key
//! reaches the workload as typed, and restored however the command ends; and its size, which an
//! interactive console is resized to.

use rustix::termios::{OptionalActions, Termios, isatty, tcgetattr, tcgetwinsize, tcsetattr};
use std::io::{stdin, stdout};

/// The terminal in raw mode, restored when dropped.
pub struct Raw {
    saved: Termios,
}

impl std::fmt::Debug for Raw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Raw").finish_non_exhaustive()
    }
}

impl Raw {
    /// Raw mode, if standard input and output are a terminal.
    #[must_use]
    pub fn enter() -> Option<Self> {
        if !isatty(stdin()) || !isatty(stdout()) {
            return None;
        }
        let saved = tcgetattr(stdin()).ok()?;
        let mut raw = saved.clone();
        raw.make_raw();
        tcsetattr(stdin(), OptionalActions::Now, &raw).ok()?;
        Some(Self { saved })
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = tcsetattr(stdin(), OptionalActions::Now, &self.saved);
    }
}

/// The terminal's columns and rows, if standard output is one.
#[must_use]
pub fn size() -> Option<(u16, u16)> {
    let w = tcgetwinsize(stdout()).ok()?;
    (w.ws_col > 0 && w.ws_row > 0).then_some((w.ws_col, w.ws_row))
}
