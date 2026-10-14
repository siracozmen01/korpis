// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which backups a prune forgets (§5.3 of docs/06-storage.md), and why each other one is kept.
//! Each volume's backups are judged on their own, since a backup is of one volume: a whole backup
//! is kept if it is one of its volume's newest `keep_last`, or the newest of its hour, day, ISO
//! week, month, or year among the newest that many of each that have a backup, in UTC. So any
//! policy that keeps something keeps each volume's newest whole backup, which its next backup
//! takes unchanged files from.
//! Whatever the policy says, a backup younger than `min_retention` is kept, as is one still
//! pending, and one an open plan restores (Finding 35 of docs/23-walkthroughs.md); a failed one
//! is kept for `keep_failed`, so its reason can still be read. Everything else is forgotten.

use super::calendar;
use super::{BackupStatus, Retention};
use std::collections::BTreeSet;
use std::time::SystemTime;
use uuid::Uuid;

/// A backup, as retention judges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// The backup.
    pub id: Uuid,
    /// The volume it is of.
    pub volume: Uuid,
    /// When it was taken.
    pub created: SystemTime,
    /// Whether it is whole.
    pub status: BackupStatus,
}

/// Why a backup is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kept {
    /// One of the newest `keep_last`.
    Last,
    /// The newest of its hour, among the newest `keep_hourly` hours.
    Hourly,
    /// Of its day.
    Daily,
    /// Of its ISO week.
    Weekly,
    /// Of its month.
    Monthly,
    /// Of its year.
    Yearly,
    /// Younger than the floor.
    Floor,
    /// An open plan restores it.
    Restoring,
    /// Not finished.
    Pending,
    /// Failed, more recently than `keep_failed`.
    RecentFailure,
}

impl Kept {
    /// The stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Last => "last",
            Self::Hourly => "hourly",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
            Self::Floor => "min_retention",
            Self::Restoring => "restoring",
            Self::Pending => "pending",
            Self::RecentFailure => "keep_failed",
        }
    }
}

/// What retention decided of one backup: why it is kept, or nothing when it is forgotten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    /// The backup.
    pub id: Uuid,
    /// Why it is kept; empty when it is forgotten.
    pub kept: BTreeSet<Kept>,
}

/// The newest of each period `bucket` names, among the newest `count` periods with a backup.
fn periods<K: Ord>(whole: &[Candidate], count: u32, bucket: impl Fn(SystemTime) -> K) -> Vec<Uuid> {
    let mut seen = BTreeSet::new();
    let mut kept = Vec::new();
    for c in whole {
        if u32::try_from(seen.len()).unwrap_or(u32::MAX) >= count {
            break;
        }
        if seen.insert(bucket(c.created)) {
            kept.push(c.id);
        }
    }
    kept
}

/// The whole backups of volume `volume` each rule of `policy` keeps.
fn by_rule(policy: &Retention, backups: &[Candidate], volume: Uuid) -> Vec<(Kept, Vec<Uuid>)> {
    let mut whole: Vec<Candidate> = backups
        .iter()
        .filter(|c| c.volume == volume && c.status == BackupStatus::Succeeded)
        .copied()
        .collect();
    whole.sort_by(|a, b| b.created.cmp(&a.created).then(b.id.cmp(&a.id)));
    let last = whole.iter().take(usize::try_from(policy.keep_last).unwrap_or(usize::MAX));
    vec![
        (Kept::Last, last.map(|c| c.id).collect()),
        (Kept::Hourly, periods(&whole, policy.keep_hourly, calendar::hour)),
        (Kept::Daily, periods(&whole, policy.keep_daily, calendar::day)),
        (Kept::Weekly, periods(&whole, policy.keep_weekly, calendar::week)),
        (Kept::Monthly, periods(&whole, policy.keep_monthly, calendar::month)),
        (Kept::Yearly, periods(&whole, policy.keep_yearly, calendar::year)),
    ]
}

/// Judges `backups` under `policy` at `now`, with those in `restoring` read by an open plan; in
/// the order they were given.
#[must_use]
pub fn judge(
    policy: &Retention,
    backups: &[Candidate],
    restoring: &BTreeSet<Uuid>,
    now: SystemTime,
) -> Vec<Judged> {
    let volumes: BTreeSet<Uuid> = backups.iter().map(|c| c.volume).collect();
    let by_rule: Vec<(Kept, Vec<Uuid>)> =
        volumes.into_iter().flat_map(|v| by_rule(policy, backups, v)).collect();
    let age = |c: &Candidate| now.duration_since(c.created).unwrap_or_default();
    backups
        .iter()
        .map(|c| {
            let mut kept: BTreeSet<Kept> =
                by_rule.iter().filter(|(_, ids)| ids.contains(&c.id)).map(|(k, _)| *k).collect();
            if age(c) < policy.min_retention {
                kept.insert(Kept::Floor);
            }
            if restoring.contains(&c.id) {
                kept.insert(Kept::Restoring);
            }
            match c.status {
                BackupStatus::Pending => {
                    kept.insert(Kept::Pending);
                }
                BackupStatus::Failed if age(c) < policy.keep_failed => {
                    kept.insert(Kept::RecentFailure);
                }
                BackupStatus::Failed | BackupStatus::Succeeded => {}
            }
            Judged { id: c.id, kept }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    const HOUR: u64 = 3600;
    const DAY: u64 = 24 * HOUR;

    /// 2026-10-14 00:00 UTC, a Wednesday, and `hours` after it.
    fn at(hours: i64) -> SystemTime {
        let base = UNIX_EPOCH + Duration::from_secs(20_740 * DAY);
        let offset = Duration::from_secs(hours.unsigned_abs() * HOUR);
        if hours < 0 { base - offset } else { base + offset }
    }

    fn whole(n: u128, hours: i64) -> Candidate {
        Candidate {
            id: Uuid::from_u128(n),
            volume: Uuid::from_u128(100),
            created: at(hours),
            status: BackupStatus::Succeeded,
        }
    }

    #[test]
    fn each_volume_keeps_its_own_newest() {
        let mut other = whole(3, -2);
        other.volume = Uuid::from_u128(200);
        let backups = [whole(1, 0), whole(2, -1), other];
        let last = Retention { keep_last: 1, ..Retention::default() };
        assert_eq!(kept(&last, &backups, at(0)), [1, 3], "one of each volume");
    }

    fn kept(policy: &Retention, backups: &[Candidate], now: SystemTime) -> Vec<u128> {
        let judged = judge(policy, backups, &BTreeSet::new(), now);
        judged.iter().filter(|j| !j.kept.is_empty()).map(|j| j.id.as_u128()).collect()
    }

    #[test]
    fn the_newest_of_each_period_is_kept_among_the_newest_periods() {
        // Two a day for five days, at 01:00 and 13:00.
        let backups: Vec<Candidate> = (0..10)
            .map(|i| {
                whole(u128::try_from(i).unwrap(), -24 * (i / 2) - if i % 2 == 0 { 11 } else { 23 })
            })
            .collect();
        let daily = Retention { keep_daily: 3, ..Retention::default() };
        assert_eq!(
            kept(&daily, &backups, at(0)),
            [0, 2, 4],
            "the later of each of the last three days"
        );
        let last = Retention { keep_last: 2, keep_daily: 3, ..Retention::default() };
        assert_eq!(kept(&last, &backups, at(0)), [0, 1, 2, 4]);
        let judged = judge(&last, &backups, &BTreeSet::new(), at(0));
        assert_eq!(judged[0].kept, BTreeSet::from([Kept::Last, Kept::Daily]));
    }

    #[test]
    fn weeks_and_years_bucket_by_the_calendar() {
        // Monday 2026-10-12, Sunday 2026-10-11, and a year before.
        let backups = [whole(1, -48), whole(2, -49), whole(3, -24 * 365)];
        let weekly = Retention { keep_weekly: 2, ..Retention::default() };
        assert_eq!(kept(&weekly, &backups, at(0)), [1, 2], "Monday and Sunday are two weeks");
        let yearly = Retention { keep_yearly: 5, ..Retention::default() };
        assert_eq!(kept(&yearly, &backups, at(0)), [1, 3]);
    }

    #[test]
    fn the_floor_restores_and_unfinished_backups_are_kept_whatever_the_policy() {
        let policy = Retention {
            keep_last: 1,
            min_retention: Duration::from_secs(2 * DAY),
            keep_failed: Duration::from_secs(DAY),
            ..Retention::default()
        };
        let mut failed_old = whole(5, -72);
        failed_old.status = BackupStatus::Failed;
        let mut failed_new = whole(6, -1);
        failed_new.status = BackupStatus::Failed;
        let mut pending = whole(7, -100);
        pending.status = BackupStatus::Pending;
        let backups = [
            whole(1, 0),
            whole(2, -30),
            whole(3, -100),
            whole(4, -200),
            failed_old,
            failed_new,
            pending,
        ];
        let restoring = BTreeSet::from([Uuid::from_u128(4)]);
        let judged = judge(&policy, &backups, &restoring, at(0));
        let reasons: Vec<Vec<&str>> =
            judged.iter().map(|j| j.kept.iter().map(|k| k.as_str()).collect()).collect();
        assert_eq!(
            reasons,
            [
                vec!["last", "min_retention"],
                vec!["min_retention"],
                vec![],
                vec!["restoring"],
                vec![],
                vec!["min_retention", "keep_failed"],
                vec!["pending"],
            ]
        );
    }
}
