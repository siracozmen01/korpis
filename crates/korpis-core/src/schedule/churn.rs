// SPDX-License-Identifier: AGPL-3.0-or-later

//! The churn signal (§2.2 of docs/05-scheduling.md): a node that recently lost its workloads may
//! be unhealthy. What it counts is the node's loss events, each time it went unheard while it held
//! workloads, one event however many it held; a workload's own crashes are not counted, because
//! they say more about the recipe than the machine and restart backoff already answers them. Each
//! event is remembered with a half-life of a day and forgotten after a week.

use std::time::Duration;

/// How long until a loss weighs half as much.
pub const CHURN_HALF_LIFE: Duration = Duration::from_secs(86_400);

/// How long a loss is remembered at all.
pub const CHURN_MEMORY: Duration = Duration::from_secs(7 * 86_400);

/// What losses of these ages weigh together now: each 1 when it happened, halving every
/// [`CHURN_HALF_LIFE`], and nothing past [`CHURN_MEMORY`].
#[must_use]
pub fn loss_weight(ages: impl IntoIterator<Item = Duration>) -> f64 {
    ages.into_iter()
        .filter(|age| *age <= CHURN_MEMORY)
        .map(|age| 0.5_f64.powf(age.as_secs_f64() / CHURN_HALF_LIFE.as_secs_f64()))
        .sum()
}

/// The signal for losses weighing `weight`: 1 for none, a half for one fresh loss, toward 0.
#[must_use]
pub fn churn_score(weight: f64) -> f64 {
    1.0 / (1.0 + weight.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loss_fades_by_half_each_day_and_is_forgotten_after_a_week() {
        let day = CHURN_HALF_LIFE;
        assert!((loss_weight([Duration::ZERO]) - 1.0).abs() < 1e-9);
        assert!((loss_weight([day]) - 0.5).abs() < 1e-9);
        assert!((loss_weight([Duration::ZERO, day * 2]) - 1.25).abs() < 1e-9);
        assert!(loss_weight([day * 8]).abs() < 1e-9, "forgotten");
        assert!((churn_score(0.0) - 1.0).abs() < 1e-9);
        assert!((churn_score(1.0) - 0.5).abs() < 1e-9);
    }
}
