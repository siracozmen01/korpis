// SPDX-License-Identifier: AGPL-3.0-or-later

//! How the in-memory host's jobs end: at once, as each kind's record says it ended.

use korpis_proto::korpis::backup::v1 as backup;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use std::collections::BTreeSet;

/// The status of `request`'s job once it finished as the in-memory host finishes jobs, of which
/// `blocks` are the block devices: a block device's backup holds an `ext4` image, and a reading
/// restore names where the image it restored would be.
pub(super) fn at_once(
    request: &helper::StartJobRequest,
    blocks: &BTreeSet<String>,
) -> helper::JobStatusResponse {
    let mut image = String::new();
    let mut result = backup::JobResult { ok: true, ..Default::default() };
    match &request.work {
        Some(Work::Backup(b)) => {
            let stats = backup::BackupStats { files: 1, ..Default::default() };
            result.backup = backup::Backup {
                id: b.backup.clone(),
                consistent: b.take_snapshot || !b.snapshot.is_empty(),
                stats: stats.into(),
                image_filesystem: if blocks.contains(&b.volume) {
                    "ext4".into()
                } else {
                    String::new()
                },
                ..Default::default()
            }
            .into();
        }
        Some(Work::Restore(r)) => {
            if r.reading {
                image = format!("/memory/.{}.read+{}/block.ext4", r.volume, request.job);
            }
            result.restored = backup::Restored { verified: true, ..Default::default() }.into();
        }
        Some(Work::Install(w)) => {
            // Every action runs, and nothing is written: the in-memory host has no files.
            let done = u32::try_from(w.actions.len()).unwrap_or(u32::MAX);
            let install = korpis_proto::korpis::install::v1::InstallResult {
                ok: true,
                done,
                ..Default::default()
            };
            return helper::JobStatusResponse {
                state: helper::JobState::JOB_STATE_FINISHED.into(),
                install: install.into(),
                progress: done,
                ..Default::default()
            };
        }
        None => result.ok = false,
    }
    helper::JobStatusResponse {
        state: helper::JobState::JOB_STATE_FINISHED.into(),
        result: result.into(),
        image,
        ..Default::default()
    }
}
