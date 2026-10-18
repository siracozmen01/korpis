// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a `task` or `scheduled` workload's runs are made, ended, and kept (§4 of
//! docs/05-scheduling.md). The defaults are the values that can neither run two runs against one
//! volume nor fire a herd of missed ones: `forbid`, and no catching up.

use super::SpecError;
use std::time::Duration;

/// The most missed occurrences caught up at once; older ones are recorded as one missed run
/// (§4.3 of docs/05-scheduling.md).
pub const MAX_CATCH_UP: u32 = 100;
/// The most retries a policy may ask for.
pub const MAX_RETRIES: u32 = 100;
/// The most runs of either kind a policy may keep: each kept run is a stream on its node.
pub const MAX_KEPT: u32 = 1000;
/// The most runs `allow` may keep active at once: each active run is a stream on its node, as
/// each kept one is, so the disk quota can count them (§4.3 of docs/05-scheduling.md).
pub const MAX_ACTIVE: u32 = 100;
/// How many runs `allow` keeps active at once when its policy says nothing.
pub const ALLOW_ACTIVE: u32 = 10;
/// The longest backoff a policy may ask for.
pub const MAX_BACKOFF: Duration = Duration::from_secs(86_400);

/// What a run due while another is active does (§4.3 of docs/05-scheduling.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Concurrency {
    /// It is recorded missed, and the active one goes on.
    #[default]
    Forbid,
    /// The active one is cancelled, and it is made.
    Replace,
    /// It is made beside the active one.
    Allow,
}

/// A workload's run policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunPolicy {
    /// What a run due while another is active does.
    pub concurrency: Concurrency,
    /// How many times a failed run is tried again.
    pub retries: u32,
    /// How long before the first retry; each later one waits twice the one before.
    pub backoff: Duration,
    /// How long a run may take before its node stops it; unbounded when absent.
    pub timeout: Option<Duration>,
    /// How many succeeded runs are kept.
    pub keep_successful: u32,
    /// How many runs that ended any other way are kept.
    pub keep_failed: u32,
    /// Whether occurrences missed while the control plane was down are run, oldest first.
    pub catch_up: bool,
    /// The most runs active at once: one under `forbid` and `replace`, and under `allow` what
    /// the policy says, [`ALLOW_ACTIVE`] when it says nothing. A run due past it is missed.
    pub max_active: u32,
}

impl Default for RunPolicy {
    fn default() -> Self {
        Self {
            concurrency: Concurrency::Forbid,
            retries: 0,
            backoff: Duration::from_secs(10),
            timeout: None,
            keep_successful: 3,
            keep_failed: 10,
            catch_up: false,
            max_active: 1,
        }
    }
}

impl RunPolicy {
    /// Checks its bounds.
    ///
    /// # Errors
    /// A count or a duration out of range, named by its field.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.retries > MAX_RETRIES {
            return Err(SpecError::new("runs.retries", format!("at most {MAX_RETRIES}")));
        }
        if self.backoff < Duration::from_secs(1) || self.backoff > MAX_BACKOFF {
            return Err(SpecError::new("runs.backoff", "between a second and a day"));
        }
        if self.timeout.is_some_and(|t| t < Duration::from_secs(1)) {
            return Err(SpecError::new("runs.timeout", "at least a second"));
        }
        for (field, n) in
            [("runs.keep_successful", self.keep_successful), ("runs.keep_failed", self.keep_failed)]
        {
            if n > MAX_KEPT {
                return Err(SpecError::new(field, format!("at most {MAX_KEPT}")));
            }
        }
        match self.concurrency {
            Concurrency::Allow if !(1..=MAX_ACTIVE).contains(&self.max_active) => {
                Err(SpecError::new("runs.max_active", format!("between 1 and {MAX_ACTIVE}")))
            }
            Concurrency::Forbid | Concurrency::Replace if self.max_active != 1 => {
                Err(SpecError::new("runs.max_active", "only with concurrency allow"))
            }
            _ => Ok(()),
        }
    }

    /// The most runs whose console streams the node holds at once: those kept of either kind
    /// and those active (§4 of docs/14-streams.md). What the disk quota counts a workload's
    /// streams by.
    #[must_use]
    pub const fn streams_held(&self) -> u32 {
        self.keep_successful.saturating_add(self.keep_failed).saturating_add(self.max_active)
    }

    /// How long the `n`-th retry waits after the failure it follows: the backoff, doubled for
    /// each retry before it, at most a day.
    #[must_use]
    pub fn retry_after(&self, n: u32) -> Duration {
        let doubled = 2u32.checked_pow(n.saturating_sub(1)).unwrap_or(u32::MAX);
        self.backoff.saturating_mul(doubled).min(MAX_BACKOFF)
    }
}

/// Checks a time zone's name: what an IANA zone's name can be. Whether the zone exists is the
/// control plane's to say, from its zone database.
pub(super) fn validate_zone(name: &str) -> Result<(), SpecError> {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+');
    if name.is_empty() || name.len() > 64 || !name.chars().all(plain) || name.contains("..") {
        return Err(SpecError::new("timezone", "an IANA zone name, such as Europe/Berlin"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_valid_and_bounds_are_named_by_field() {
        assert!(RunPolicy::default().validate().is_ok());
        let over = RunPolicy { retries: MAX_RETRIES + 1, ..RunPolicy::default() };
        assert_eq!(over.validate().unwrap_err().field, "runs.retries");
        let instant = RunPolicy { timeout: Some(Duration::ZERO), ..RunPolicy::default() };
        assert_eq!(instant.validate().unwrap_err().field, "runs.timeout");
        let kept = RunPolicy { keep_failed: MAX_KEPT + 1, ..RunPolicy::default() };
        assert_eq!(kept.validate().unwrap_err().field, "runs.keep_failed");
    }

    #[test]
    fn only_allow_keeps_more_than_one_run_active_and_never_past_the_bound() {
        let allow = |max_active| RunPolicy {
            concurrency: Concurrency::Allow,
            max_active,
            ..RunPolicy::default()
        };
        assert!(allow(ALLOW_ACTIVE).validate().is_ok());
        assert_eq!(allow(0).validate().unwrap_err().field, "runs.max_active");
        assert_eq!(allow(MAX_ACTIVE + 1).validate().unwrap_err().field, "runs.max_active");
        let forbid = RunPolicy { max_active: 2, ..RunPolicy::default() };
        assert_eq!(forbid.validate().unwrap_err().field, "runs.max_active");
        assert_eq!(RunPolicy::default().streams_held(), 3 + 10 + 1);
        assert_eq!(allow(4).streams_held(), 3 + 10 + 4);
    }

    #[test]
    fn each_retry_waits_twice_the_one_before_and_never_more_than_a_day() {
        let p = RunPolicy::default();
        assert_eq!(p.retry_after(1), Duration::from_secs(10));
        assert_eq!(p.retry_after(3), Duration::from_secs(40));
        assert_eq!(p.retry_after(40), MAX_BACKOFF);
    }

    #[test]
    fn a_zone_name_is_checked_for_its_shape_alone() {
        for ok in ["UTC", "Europe/Berlin", "America/Argentina/Buenos_Aires", "Etc/GMT+3"] {
            assert!(validate_zone(ok).is_ok(), "{ok}");
        }
        for bad in ["", "../etc/passwd", "Europe Berlin", "Europe/Berlin\n"] {
            assert!(validate_zone(bad).is_err(), "{bad}");
        }
    }
}
