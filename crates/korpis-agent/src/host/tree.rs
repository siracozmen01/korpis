// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the agent asks of the helper for a migration's tree passes (§8.1 of
//! docs/05-scheduling.md): sending a pass of a volume's tree, receiving one into the
//! destination's copy, and reading how either ended. Each side speaks on a socket the agent
//! joins to the transfer connection.

use super::HostError;
use super::node::NodeHost;
use korpis_proto::korpis::helper::v1 as helper;
use std::future::Future;
use std::os::fd::OwnedFd;

/// The helper's tree verbs.
pub trait Trees: Send + Sync + 'static {
    /// Starts sending the pass `r` names of a volume here; the socket the sender speaks on.
    fn send_tree(
        &self,
        r: &helper::SendTreeRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send;

    /// Starts receiving the pass `r` names into a volume here; the socket the receiver speaks on.
    fn receive_tree(
        &self,
        r: &helper::ReceiveTreeRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send;

    /// How the pass `r` names came to, on the side it names.
    fn tree_status(
        &self,
        r: &helper::TreeStatusRequest,
    ) -> impl Future<Output = Result<helper::TreeStatusResponse, HostError>> + Send;
}

impl Trees for NodeHost {
    fn send_tree(
        &self,
        r: &helper::SendTreeRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send {
        self.helper().send_tree(r)
    }

    fn receive_tree(
        &self,
        r: &helper::ReceiveTreeRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send {
        self.helper().receive_tree(r)
    }

    fn tree_status(
        &self,
        r: &helper::TreeStatusRequest,
    ) -> impl Future<Output = Result<helper::TreeStatusResponse, HostError>> + Send {
        self.helper().tree_status(r)
    }
}
