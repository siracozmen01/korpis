// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tree verbs (§8.1 of docs/05-scheduling.md): each side's socket is the one descriptor the
//! helper's answer carries.

use super::{HelperClient, failed};
use crate::host::HostError;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use std::os::fd::OwnedFd;

/// The one descriptor an answer carries, or why the answer is not one.
fn the_socket(answered: (Answer, Vec<OwnedFd>)) -> Result<OwnedFd, HostError> {
    match answered {
        (Answer::SendTree(_) | Answer::ReceiveTree(_), mut fds) if fds.len() == 1 => {
            fds.pop().ok_or_else(|| failed("no socket"))
        }
        (other, fds) => Err(failed(format!("answered {other:?} with {} descriptors", fds.len()))),
    }
}

impl HelperClient {
    /// Starts sending the pass `r` names; the socket the sender speaks on.
    pub async fn send_tree(&self, r: &helper::SendTreeRequest) -> Result<OwnedFd, HostError> {
        the_socket(self.call_attached(Verb::SendTree(Box::new(r.clone()))).await?)
    }

    /// Starts receiving the pass `r` names; the socket the receiver speaks on.
    pub async fn receive_tree(&self, r: &helper::ReceiveTreeRequest) -> Result<OwnedFd, HostError> {
        the_socket(self.call_attached(Verb::ReceiveTree(Box::new(r.clone()))).await?)
    }

    /// How the pass `r` names came to.
    pub async fn tree_status(
        &self,
        r: &helper::TreeStatusRequest,
    ) -> Result<helper::TreeStatusResponse, HostError> {
        match self.call(Verb::TreeStatus(Box::new(r.clone()))).await? {
            Answer::TreeStatus(a) => Ok(*a),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }
}
