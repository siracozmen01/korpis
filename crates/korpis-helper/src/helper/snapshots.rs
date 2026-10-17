// SPDX-License-Identifier: AGPL-3.0-or-later

//! The snapshot verbs (§4 of docs/06-storage.md): each on the class that holds its volume.

use super::Helper;
use crate::error::{HelperError, HelperResult};
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;

impl Helper {
    /// The answer to a snapshot verb.
    pub(super) fn snapshot_verb(&mut self, verb: Verb) -> HelperResult<Answer> {
        Ok(match verb {
            Verb::SnapshotVolume(r) => {
                let created = self.holding(&r.volume)?.snapshot(&r.volume, &r.snapshot)?;
                Answer::SnapshotVolume(Box::new(helper::SnapshotVolumeResponse {
                    created,
                    ..Default::default()
                }))
            }
            Verb::RollbackVolume(r) => {
                self.holding(&r.volume)?.rollback(&r.volume, &r.snapshot)?;
                Answer::RollbackVolume(Box::default())
            }
            Verb::DeleteSnapshot(r) => {
                let deleted = match self.holding(&r.volume) {
                    Ok(c) => c.delete_snapshot(&r.volume, &r.snapshot)?,
                    Err(HelperError::NotFound(_)) => false,
                    Err(e) => return Err(e),
                };
                Answer::DeleteSnapshot(Box::new(helper::DeleteSnapshotResponse {
                    deleted,
                    ..Default::default()
                }))
            }
            _ => return Err(HelperError::Refused("not a snapshot verb".into())),
        })
    }
}
