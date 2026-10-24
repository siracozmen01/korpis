// SPDX-License-Identifier: AGPL-3.0-or-later

//! A command run inside a running object (§4.2 of docs/04-runtimes.md), as every driver bounds
//! it: its arguments, how long it may run, and how much of its output is sent; and how its run is
//! reported, its output as it comes and then how it ended.

use korpis_proto::korpis::driver::v1 as driver;
use std::time::Duration;

/// The longest a command may run.
pub const MOST_TIMEOUT: Duration = Duration::from_secs(600);
/// The most of a command's output sent.
pub const MOST_OUTPUT: u32 = 64 * 1024;
/// The most arguments a command has, and the longest one.
const MOST_ARGS: usize = 256;
const LONGEST_ARG: usize = 4096;

/// A command to run, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    argv: Vec<String>,
    timeout: Duration,
    output_limit: usize,
}

/// What a command's run sends, in order: its output, then how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    /// What it wrote.
    Output(Vec<u8>),
    /// How it ended: its status, whether it was killed for its timeout, and whether output was
    /// dropped.
    Exit {
        /// Its exit status; 128 and the signal's number for one a signal ended.
        status: u32,
        /// Killed for outliving its timeout.
        timed_out: bool,
        /// Output beyond the limit was dropped.
        truncated: bool,
    },
}

impl Command {
    /// `argv` run for at most `timeout`, sending at most `output_limit` bytes of its output; a
    /// command with no arguments, too many or too long, one holding a NUL, a timeout of zero or
    /// past ten minutes, or a limit past 64 KiB is refused.
    ///
    /// # Errors
    /// Each refusal says which.
    pub fn checked(
        argv: Vec<String>,
        timeout: Duration,
        output_limit: u32,
    ) -> Result<Self, String> {
        let refused = |why: &str| Err(why.to_owned());
        if argv.is_empty() || argv.len() > MOST_ARGS {
            return refused("a command is one to 256 arguments");
        }
        if argv.iter().any(|a| a.len() > LONGEST_ARG || a.contains('\0')) {
            return refused("an argument is at most 4096 bytes, and holds no NUL");
        }
        if timeout.is_zero() || timeout > MOST_TIMEOUT {
            return refused("a command's timeout is more than zero and at most ten minutes");
        }
        if output_limit > MOST_OUTPUT {
            return refused("a command's output is limited to at most 64 KiB");
        }
        let output_limit = usize::try_from(output_limit).unwrap_or(0);
        Ok(Self { argv, timeout, output_limit })
    }

    /// The command and its arguments.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// How long it may run.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The most of its output sent.
    #[must_use]
    pub const fn output_limit(&self) -> usize {
        self.output_limit
    }
}

/// One event of a command's run, as the protocol sends it.
#[must_use]
pub fn response(ran: Ran) -> driver::ExecResponse {
    use driver::__buffa::oneof::exec_response::Event;
    let event = match ran {
        Ran::Output(data) => Event::Output(data),
        Ran::Exit { status, timed_out, truncated } => {
            let exit = driver::ExecExit { status, timed_out, truncated, ..Default::default() };
            Event::Exit(Box::new(exit))
        }
    };
    driver::ExecResponse { event: Some(event), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_bounded_in_its_arguments_its_time_and_its_output() {
        let second = Duration::from_secs(1);
        let ok = |argv: &[&str], t, limit| {
            Command::checked(argv.iter().map(|a| (*a).to_owned()).collect(), t, limit).is_ok()
        };
        assert!(ok(&["pg_isready", "-q"], second, 4096));
        assert!(!ok(&[], second, 4096), "nothing to run");
        assert!(!ok(&["a\0b"], second, 4096), "a NUL");
        assert!(!ok(&["true"], Duration::ZERO, 4096), "no time");
        assert!(!ok(&["true"], MOST_TIMEOUT + second, 4096), "past ten minutes");
        assert!(!ok(&["true"], second, MOST_OUTPUT + 1), "past 64 KiB");
        let long = "x".repeat(LONGEST_ARG + 1);
        assert!(!ok(&[long.as_str()], second, 0), "one argument too long");
    }
}
