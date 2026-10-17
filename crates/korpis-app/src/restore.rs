// SPDX-License-Identifier: AGPL-3.0-or-later

//! Restoring the control plane from its backup (§8 of docs/03-state.md, §6 of
//! docs/18-operations.md). Its first step fences lease epochs: a restored store remembers the
//! epochs of the backup, and nodes hold newer ones, so the floor is raised past every epoch the
//! issue rate allowed since the backup, and a settling window opens in which nothing is issued
//! and each reconnecting node presents the assignments it holds, whose signed epochs raise the
//! floor further. A presented epoch is believed only when its signature verifies against the
//! assignment keyring, since an epoch is authority.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, EffectWrite, LeaseWrite, RecipeSource, Sealed, Store, Tx};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::lease::fence_floor;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// What the backup said of epochs, as the control-plane backup keeps it (§6 of
/// docs/18-operations.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupEpochs {
    /// The highest epoch issued when the backup was taken.
    pub max_issued: u64,
    /// The issue-rate cap then, per second.
    pub rate_per_sec: u64,
    /// When the backup was taken.
    pub taken_at: SystemTime,
}

/// The fence a restore set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fenced {
    /// The epoch floor: nothing at or below it is issued again.
    pub floor: u64,
    /// Until when nothing is issued at all.
    pub settling_until: SystemTime,
}

/// What a node's presentation came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Presentation {
    /// Assignments whose signature verified and that were issued to the node presenting them.
    pub verified: usize,
    /// Assignments believed in nothing: a signature that does not verify, a key the keyring
    /// does not hold, or another node's.
    pub refused: usize,
    /// The floor the node's epochs raised it to, if they raised it.
    pub raised_to: Option<u64>,
}

fn seconds(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// A restore's first fence: the floor past every epoch issued since `backup`, and a window of
    /// `settle` in which nothing is issued, recorded as asked at the machine by `operator`.
    pub async fn fence_epochs(
        &self,
        backup: BackupEpochs,
        settle: Duration,
        operator: &str,
    ) -> Result<Fenced, AppError> {
        let now = self.clock.now();
        let age = now.duration_since(backup.taken_at).unwrap_or(Duration::ZERO);
        let floor = fence_floor(backup.max_issued, backup.rate_per_sec, age);
        let settling_until = now + settle;
        let mut tx = self.store.begin().await?;
        tx.fence_epochs(floor, settling_until).await?;
        let effect = Effect::new(
            Principal::System(SystemActor::Bootstrap),
            "restore.epochs.fenced",
            Outcome::Succeeded,
        )
        .after(details([
            ("floor", floor.into()),
            ("settling_until", seconds(settling_until).into()),
            ("backup_max_issued", backup.max_issued.into()),
            ("backup_rate_per_sec", backup.rate_per_sec.into()),
            ("backup_age_seconds", age.as_secs().into()),
            ("operator", operator.into()),
        ]));
        tx.append_effect(&effect).await?;
        tx.commit().await?;
        Ok(Fenced { floor, settling_until })
    }

    /// Until when a restore settles, if it does: a node is asked to present while it does.
    pub async fn settling(&self) -> Result<Option<SystemTime>, AppError> {
        let mut tx = self.store.begin().await?;
        let until = tx.settling(self.clock.now()).await?;
        tx.commit().await?;
        Ok(until)
    }

    /// What `node` presented: every assignment it holds, as signed. The highest lease epoch of
    /// those that verify and were issued to it raises the epoch floor.
    pub async fn present(&self, node: Uuid, held: &[Sealed]) -> Result<Presentation, AppError> {
        let sealer = self
            .assignment_sealer
            .as_ref()
            .ok_or_else(|| AppError::Internal("no assignment sealer is configured".into()))?;
        let keys = self.store.verify_keys("assignment").await?;
        let mut presentation = Presentation::default();
        let mut highest = 0;
        for sealed in held {
            match sealer.open(sealed, &keys) {
                Ok(opened) if opened.node == node => {
                    presentation.verified += 1;
                    highest = highest.max(opened.lease_epoch);
                }
                Ok(_) | Err(_) => presentation.refused += 1,
            }
        }
        if highest == 0 {
            return Ok(presentation);
        }
        let mut tx = self.store.begin().await?;
        if tx.raise_epoch_floor(highest).await? {
            presentation.raised_to = Some(highest);
            let mut effect = Effect::new(
                Principal::System(SystemActor::Bootstrap),
                "restore.epoch_floor.raised",
                Outcome::Succeeded,
            )
            .after(details([("floor", highest.into()), ("node", node.to_string().into())]));
            effect.node = Some(node);
            tx.append_effect(&effect).await?;
        }
        tx.commit().await?;
        Ok(presentation)
    }
}
