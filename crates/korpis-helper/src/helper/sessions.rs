// SPDX-License-Identifier: AGPL-3.0-or-later

//! The file session verb (§3.2 of docs/06-storage.md): the worker's spec placed from what the
//! request names, as an install's is, the worker started, and its socket attached to the answer.

use super::Helper;
use crate::error::{HelperError, HelperResult};
use crate::files::Files;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;

impl Helper {
    /// The same helper, opening file sessions with `files`.
    #[must_use]
    pub fn with_files(mut self, files: Files) -> Self {
        self.files = Some(files);
        self
    }

    /// Opens the file session `r` asks for; its socket is attached to the answer.
    pub(super) fn open_files(&mut self, r: &helper::OpenFilesRequest) -> HelperResult<Answer> {
        if self.files.is_none() {
            return Err(HelperError::Refused("this helper opens no file sessions".into()));
        }
        let classes = &self.classes;
        let spec = crate::files::place(&self.ledger, r, |v| {
            crate::error::identifier("volume", v)?;
            classes
                .iter()
                .map(|c| c.path_of(v))
                .find(|p| p.symlink_metadata().is_ok())
                .ok_or_else(|| HelperError::NotFound(format!("volume {v}")))
        })?;
        let files = self
            .files
            .as_mut()
            .ok_or_else(|| HelperError::Refused("this helper opens no file sessions".into()))?;
        self.attached = vec![files.open(spec)?];
        Ok(Answer::OpenFiles(Box::new(helper::OpenFilesResponse {
            attached: 1,
            ..Default::default()
        })))
    }
}
