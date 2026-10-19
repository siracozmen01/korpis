// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the recipe knows about the software's behaviour: log lines that mean something, and how
//! to stop it cleanly (§3 of docs/09-recipes.md).

use super::RecipeError;
use regex::Regex;
use std::time::Duration;

/// How serious an emitted event is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Level {
    /// Informational.
    #[default]
    Info,
    /// A warning.
    Warn,
    /// An error.
    Error,
}

/// A log line that means something.
#[derive(Debug, Clone)]
pub struct LogEventRule {
    /// Matched against each line.
    pub pattern: Regex,
    /// The event it emits, e.g. `ready`.
    pub emit: String,
    /// How serious it is.
    pub level: Level,
}

/// What a recipe declares about stopping. A game server sent `SIGTERM` may not flush its world;
/// writing `stop` to its console and waiting is the difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hook {
    /// Write to the console, then wait for the process to exit.
    ConsoleWrite {
        /// What to write.
        data: String,
        /// How long to wait before stopping it anyway.
        wait_for_exit: Duration,
    },
}

/// Lifecycle hooks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleHooks {
    /// Before the workload is stopped.
    pub pre_stop: Option<Hook>,
}

impl LifecycleHooks {
    /// How long the `pre_stop` hook gives the software to exit by itself; zero without one.
    #[must_use]
    pub fn stop_wait(&self) -> Duration {
        match &self.pre_stop {
            Some(Hook::ConsoleWrite { wait_for_exit, .. }) => *wait_for_exit,
            None => Duration::ZERO,
        }
    }
}

/// The longest a pre-stop hook may hold a stop.
pub const MAX_PRE_STOP: Duration = Duration::from_secs(600);

fn event_name(e: &str) -> bool {
    let mut b = e.bytes();
    b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

/// Checks event names and hook bounds.
pub fn validate(events: &[LogEventRule], hooks: &LifecycleHooks) -> Result<(), RecipeError> {
    if let Some(i) = events.iter().position(|e| !event_name(&e.emit)) {
        return Err(RecipeError::new(&format!("logs.events[{i}].emit"), "[a-z][a-z0-9_]*"));
    }
    match &hooks.pre_stop {
        Some(Hook::ConsoleWrite { data, .. }) if data.is_empty() => {
            Err(RecipeError::new("lifecycle_hooks.pre_stop", "writes something"))
        }
        Some(Hook::ConsoleWrite { wait_for_exit, .. }) if *wait_for_exit > MAX_PRE_STOP => {
            Err(RecipeError::new("lifecycle_hooks.pre_stop.wait_for_exit", "at most 10 minutes"))
        }
        _ => Ok(()),
    }
}
