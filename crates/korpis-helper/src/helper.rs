// SPDX-License-Identifier: AGPL-3.0-or-later

//! One request in, one answer out: the envelope decoded, the verb dispatched to the class or
//! the ledger it concerns, and whatever happened encoded as the answer. Nothing here panics on
//! what a peer sends; a request that does not decode, or names a verb this helper does not
//! know, is answered as such.

use crate::error::{HelperError, HelperResult};
use crate::ids::Ledger;
use crate::storage::Class;
use buffa::Message as _;
use korpis_core::workload::assignment::IdRange;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;

/// The helper's state: its classes, the first of them the default, and its ledger.
#[derive(Debug)]
pub struct Helper {
    classes: Vec<Class>,
    ledger: Ledger,
}

fn unsupported() -> helper::HelperError {
    helper::HelperError {
        code: helper::HelperErrorCode::HELPER_ERROR_CODE_UNSUPPORTED.into(),
        message: "a verb this helper does not know".into(),
        ..Default::default()
    }
}

impl Helper {
    /// A helper offering `classes`, the first the default, and keeping `ledger`.
    #[must_use]
    pub fn new(classes: Vec<Class>, ledger: Ledger) -> Self {
        Self { classes, ledger }
    }

    /// The class `name` names; the default when it names none.
    fn class(&self, name: &str) -> HelperResult<&Class> {
        let found = if name.is_empty() {
            self.classes.first()
        } else {
            self.classes.iter().find(|c| c.name() == name)
        };
        found.ok_or_else(|| HelperError::NotFound(format!("storage class {name:?}")))
    }

    /// The class that holds `volume`, if any does.
    fn holding(&self, volume: &str) -> HelperResult<&Class> {
        crate::error::identifier("volume", volume)?;
        self.classes
            .iter()
            .find(|c| c.path_of(volume).symlink_metadata().is_ok())
            .ok_or_else(|| HelperError::NotFound(format!("volume {volume}")))
    }

    fn dispatch(&mut self, verb: Verb) -> HelperResult<Answer> {
        Ok(match verb {
            Verb::EnsureVolume(r) => {
                Answer::EnsureVolume(Box::new(self.class(&r.storage_class)?.ensure(&r)?))
            }
            Verb::DeleteVolume(r) => {
                let deleted = match self.holding(&r.volume) {
                    Ok(c) => c.delete(&r.volume)?,
                    Err(HelperError::NotFound(_)) => false,
                    Err(e) => return Err(e),
                };
                Answer::DeleteVolume(Box::new(helper::DeleteVolumeResponse {
                    deleted,
                    ..Default::default()
                }))
            }
            Verb::VolumeUsage(r) => {
                Answer::VolumeUsage(Box::new(self.holding(&r.volume)?.usage(&r.volume)?))
            }
            Verb::EnsureIdRange(r) => {
                self.ledger
                    .ensure(&r.workload, IdRange { base: r.uid_base, count: r.uid_count })?;
                Answer::EnsureIdRange(Box::default())
            }
            Verb::DescribeStorage(_) => {
                let classes = self.classes.iter().map(Class::describe).collect();
                Answer::DescribeStorage(Box::new(helper::DescribeStorageResponse {
                    classes,
                    ..Default::default()
                }))
            }
        })
    }

    /// The encoded answer to the encoded request `bytes`.
    pub fn handle(&mut self, bytes: &[u8]) -> Vec<u8> {
        let answer = match helper::HelperRequest::decode_from_slice(bytes) {
            Ok(r) => match r.verb {
                Some(verb) => {
                    self.dispatch(verb).unwrap_or_else(|e| Answer::Error(Box::new(e.to_pb())))
                }
                None => Answer::Error(Box::new(unsupported())),
            },
            Err(e) => {
                tracing::warn!(error = %e, "a request that does not decode was refused");
                Answer::Error(Box::new(HelperError::Refused("undecodable request".into()).to_pb()))
            }
        };
        helper::HelperResponse { result: Some(answer), ..Default::default() }.encode_to_vec()
    }
}
