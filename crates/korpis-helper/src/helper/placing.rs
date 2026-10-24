// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a job works (§5 of docs/06-storage.md): a backup in its volume or a snapshot of it, a
//! replacing restore in a staging place, any other restore in its volume or a new directory of
//! it, and an install in the volumes its spec names, each found from the identifiers the request
//! names and nothing it says of paths.

use super::Helper;
use crate::error::{HelperError, HelperResult};
use crate::job::{DataJob, Placed, make_into};
use crate::kept::Kept;
use korpis_proto::korpis::helper::v1 as helper;

impl Helper {
    /// What backup job `job` of `b` works in: the volume, or a snapshot of it.
    pub(super) fn place_backup(
        &mut self,
        job: &str,
        b: &helper::BackupVolume,
    ) -> HelperResult<Placed> {
        use korpis_proto::korpis::backup::v1 as pb;
        use korpis_proto::korpis::backup::v1::__buffa::oneof::job_spec::Work as W;
        let (directory, taken) = self
            .holding(&b.volume)?
            .backup_source((&b.volume, &b.snapshot), (job, b.take_snapshot))?;
        let kept = taken.map(|(volume, name)| Kept::Snapshot { volume, name });
        // A block device's backup says what its image holds (§5.4 of docs/06-storage.md).
        let block = crate::block::is_block(&self.holding(&b.volume)?.path_of(&b.volume));
        let work = pb::BackupWork {
            backup: b.backup.clone(),
            parent: b.parent.clone(),
            consistent: !b.snapshot.is_empty() || b.take_snapshot,
            content: b.content.clone(),
            image_filesystem: if block { crate::block::FILESYSTEM.into() } else { String::new() },
            ..Default::default()
        };
        Ok(Placed::Data(DataJob { directory, work: W::Backup(Box::new(work)), kept }))
    }

    /// What job `r` works in, from the identifiers it names.
    pub(super) fn place(&mut self, r: &helper::StartJobRequest) -> HelperResult<Placed> {
        use korpis_proto::korpis::backup::v1 as pb;
        use korpis_proto::korpis::backup::v1::__buffa::oneof::job_spec::Work as W;
        use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work as R;
        match &r.work {
            Some(R::Backup(b)) => {
                crate::error::identifier("job", &r.job)?;
                self.place_backup(&r.job, b)
            }
            Some(R::Restore(w)) if w.replace => {
                if !(w.path.is_empty() && w.into.is_empty()) {
                    return Err(HelperError::Refused("a replacing restore is whole".into()));
                }
                let (directory, identity, limits) = self.holding(&w.volume)?.stage(&w.volume)?;
                let kept = Kept::Staging { volume: w.volume.clone(), identity, limits };
                let work = pb::RestoreWork { backup: w.backup.clone(), ..Default::default() };
                Ok(Placed::Data(DataJob {
                    directory,
                    work: W::Restore(Box::new(work)),
                    kept: Some(kept),
                }))
            }
            Some(R::Restore(w)) => {
                let volume = self.holding(&w.volume)?.path_of(&w.volume);
                crate::block::restorable(&w.volume, &volume, (&w.path, &w.into))?;
                let directory =
                    if w.into.is_empty() { volume } else { make_into(&volume, &w.into)? };
                let work = pb::RestoreWork {
                    backup: w.backup.clone(),
                    path: w.path.clone(),
                    ..Default::default()
                };
                Ok(Placed::Data(DataJob {
                    directory,
                    work: W::Restore(Box::new(work)),
                    kept: None,
                }))
            }
            Some(R::Install(w)) => {
                crate::error::identifier("job", &r.job)?;
                crate::job::install::check_ids(&self.ledger, &w.workload, w.id_base)?;
                let spec = crate::job::install::place(w, |volume| {
                    crate::block::files_of(volume, self.holding(volume)?.path_of(volume))
                })?;
                Ok(Placed::Install(Box::new(spec)))
            }
            None => Err(HelperError::Refused("a job names its work".into())),
        }
    }
}
