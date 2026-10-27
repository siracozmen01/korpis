// SPDX-License-Identifier: AGPL-3.0-or-later

//! A session reading a block device's backup, as its node asks for the job it needs (§5.4 of
//! docs/06-storage.md). The node accepted a `backup.browse` or `backup.read` session on the
//! workload and stages the backup's image for a guest of its own to read; it holds neither the
//! repository's credentials nor what says which workload a backup is of. The control plane
//! verifies the session's token again, with its own keys and through the grant the token was
//! minted from as that grant stands now, so revocation, expiry, suspension, and its conditions
//! all still decide; checks that the backup is the workload's, whole, a block device's image, and
//! of a volume the node holds; records the read as the session's subject's act under that grant,
//! as the API's browse and read are recorded (§4 of docs/15-observability.md); and grants the job
//! read access to the workload's prefix, with the volume's identifier on the node.

use super::job::{JobGrant, JobRefusal, JobTarget, Judged};
use super::read::ReadFor;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, EffectWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::Resource;
use korpis_core::authz::token::{self, Client, Verified};
use korpis_core::backup::BackupStatus;
use korpis_core::extension::AccessMode;
use korpis_core::object::{Kind, ObjectRef};
use korpis_core::principal::Principal;
use std::collections::HashSet;
use uuid::Uuid;

/// A session reading a backup, as the node that accepted it says.
#[derive(Debug, Clone)]
pub struct BackupSession {
    /// The backup.
    pub backup: Uuid,
    /// The workload the session is on.
    pub workload: Uuid,
    /// Its action.
    pub what: ReadFor,
    /// Its token, as its client presented it.
    pub token: String,
}

/// What the session's job is given: the job's grant, the volume to stage beside, and what the
/// image holds.
#[derive(Debug)]
pub struct SessionGrant {
    /// The key and access.
    pub grant: JobGrant,
    /// The volume the backup is of, by the helper's identifier on the node.
    pub volume: String,
    /// The filesystem the image holds, which the node's driver must mount to read it.
    pub image_filesystem: String,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The key and access the job reading `s`'s backup on `node` needs, the key sealed to
    /// `recipient` under tenant custody; the read is recorded first.
    pub async fn grant_backup_session(
        &self,
        node: Uuid,
        s: &BackupSession,
        recipient: &[u8],
    ) -> Judged<SessionGrant> {
        let whole = |b: &korpis_core::backup::Backup| {
            b.status == BackupStatus::Succeeded && b.deleted_at.is_none()
        };
        let Some(b) = self.store.backup(s.backup).await?.filter(whole) else {
            return Ok(Err(JobRefusal::Gone));
        };
        if b.workload != s.workload {
            return Ok(Err(JobRefusal::NotAssigned));
        }
        let Some(image_filesystem) = b.image_filesystem.clone() else {
            return Ok(Err(JobRefusal::NotAnImage));
        };
        let live = self.store.volume(b.volume).await?.filter(|v| v.deleted_at.is_none());
        let Some(volume) = live else { return Ok(Err(JobRefusal::NotAssigned)) };
        if !self.reaches(node, &volume).await? {
            return Ok(Err(JobRefusal::NotAssigned));
        }
        let Some((caller, grant)) = self.session_caller(s).await? else {
            return Ok(Err(JobRefusal::Unverified));
        };
        let mut e =
            caller.effect(s.what.action(), Outcome::Succeeded).under(grant).after(details([
                ("backup", b.id.to_string().into()),
                ("node", node.to_string().into()),
            ]));
        e.workload = Some(b.workload);
        let mut tx = self.store.begin().await?;
        tx.append_effect(&e).await?;
        tx.commit().await?;
        let repo = self.repository_of(b.repository).await?;
        let t =
            JobTarget { repo, workload: b.workload, mode: AccessMode::Read, backup: Some(b.id) };
        Ok(self.granted(node, t, recipient).await?.map(|grant| SessionGrant {
            grant,
            volume: volume.helper_id,
            image_filesystem,
        }))
    }

    /// Who opened session `s`, and the grant its token was minted from, if this control plane
    /// signed the token for the session's action on its workload and the grant permits it now;
    /// `None` otherwise. No use of a use-limited grant is spent: the token's minting spent it.
    async fn session_caller(&self, s: &BackupSession) -> Result<Option<(Caller, Uuid)>, AppError> {
        let object = ObjectRef::new(Kind::Workload, s.workload);
        let resource = Resource { chain: self.store.ancestry(object).await? };
        let now = (self.clock.now(), Client::Unseen);
        let none: HashSet<Uuid> = HashSet::new();
        let checked =
            token::verify(&s.token, &self.token_ring()?, s.what.action(), &resource, &none, now);
        let Ok(Verified { subject, grant, .. }) = checked else { return Ok(None) };
        let Ok(principal) = Principal::from_key(&subject) else { return Ok(None) };
        let caller = Caller::new(principal, None, false);
        match self.authorize_continued(&caller, s.what.action(), object, grant).await {
            Ok(_) => Ok(Some((caller, grant))),
            Err(
                AppError::Denied { .. } | AppError::NotFound { .. } | AppError::Unauthenticated,
            ) => Ok(None),
            Err(other) => Err(other),
        }
    }
}
