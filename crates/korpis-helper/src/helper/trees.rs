// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tree verbs (§8.1 of docs/05-scheduling.md): sending a pass of a volume's tree from the
//! class holding it, receiving one into a volume of the class a request names, and reading how
//! either ended. Each side's process is kept here by its volume, its pass, and its side until a
//! status reads how it ended; a helper that restarted knows of none, and says so.

use super::Helper;
use crate::error::{HelperError, HelperResult};
use crate::tree::TreeEnd;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;

impl Helper {
    /// The answer to a tree verb.
    pub(super) fn tree_verb(&mut self, verb: Verb) -> HelperResult<Answer> {
        Ok(match verb {
            Verb::SendTree(r) => {
                let key = (r.volume.clone(), r.pass.clone(), true);
                self.end_tree(&key);
                let (socket, child) = self.holding(&r.volume)?.send_tree(&r.volume)?;
                self.attached = vec![socket];
                self.trees.insert(key, child);
                let sent = helper::SendTreeResponse { attached: 1, ..Default::default() };
                Answer::SendTree(Box::new(sent))
            }
            Verb::ReceiveTree(r) => {
                let relay = std::env::current_exe()?;
                let key = (r.volume.clone(), r.pass.clone(), false);
                self.end_tree(&key);
                let class = self.class(&r.storage_class)?;
                let (socket, child) = class.receive_tree(&r.volume, &relay)?;
                self.attached = vec![socket];
                self.trees.insert(key, child);
                let got = helper::ReceiveTreeResponse { attached: 1, ..Default::default() };
                Answer::ReceiveTree(Box::new(got))
            }
            Verb::TreeStatus(r) => Answer::TreeStatus(Box::new(self.tree_status(&r)?)),
            _ => return Err(HelperError::Refused("not a tree verb".into())),
        })
    }

    /// Ends and reaps the pass `key` names, if one runs: a newer one of the same pass replaces
    /// it.
    fn end_tree(&mut self, key: &(String, String, bool)) {
        if let Some(mut old) = self.trees.remove(key) {
            let _ = old.kill();
            let _ = old.wait();
        }
    }

    /// How the pass `r` names came to.
    fn tree_status(
        &mut self,
        r: &helper::TreeStatusRequest,
    ) -> HelperResult<helper::TreeStatusResponse> {
        use helper::TreeState as S;
        let key = (r.volume.clone(), r.pass.clone(), r.sending);
        let Some(mut child) = self.trees.remove(&key) else {
            let unknown = S::TREE_STATE_UNKNOWN.into();
            return Ok(helper::TreeStatusResponse { state: unknown, ..Default::default() });
        };
        // The volume is in the class that holds it, or, gone meanwhile, in none, and the
        // default class answers for a process that only needs reaping.
        let class = match self.holding(&r.volume) {
            Ok(c) => c,
            Err(_) => self.class("")?,
        };
        let ended = class.tree_ended(&r.volume, &mut child, r.sending)?;
        let (state, sent_bytes, detail) = match ended {
            TreeEnd::Running => {
                self.trees.insert(key, child);
                (S::TREE_STATE_RUNNING, 0, String::new())
            }
            TreeEnd::Done(sent) => (S::TREE_STATE_DONE, sent, String::new()),
            TreeEnd::Failed(why) => (S::TREE_STATE_FAILED, 0, why),
        };
        Ok(helper::TreeStatusResponse {
            state: state.into(),
            sent_bytes,
            detail,
            ..Default::default()
        })
    }
}
