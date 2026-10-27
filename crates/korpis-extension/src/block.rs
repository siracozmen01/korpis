// SPDX-License-Identifier: Apache-2.0

//! Writing a block storage provider (§2.1 of docs/06-storage.md, §5 of docs/16-extensions.md).
//! One Korpis volume is one device of the storage, which the provider names. The control plane
//! asks for a device of a size, and at each placement asks for it attached to one node for one
//! lease epoch: the provider first refuses it, in the storage itself where it can, to every other
//! node, and refuses an epoch lower than one it has attached the device for, then answers with
//! what the node needs. It registers saying whether its attach fences that way
//! (`FENCING_STORAGE_ENFORCED`) or cannot (`FENCING_LEASE_ONLY`). Each call says the whole of
//! what is wanted, so a repeat changes nothing. Requires the `provider.block_storage`
//! capability.

pub use crate::provider::Failure;
use crate::provider::{Handler, Outcome, Unsupported};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use std::future::Future;

/// A block storage provider.
pub trait BlockStorage: Send + Sync + 'static {
    /// A device for `create.volume` of `create.size_bytes`: the one made for it already, if
    /// one was; the provider's name for it.
    fn create(
        &self,
        create: pb::CreateBlockVolume,
    ) -> impl Future<Output = Result<String, Failure>> + Send;

    /// Attaches `attach.device` to `attach.node` for `attach.epoch`, every other node refused
    /// first, using one of `attach.protocols`; what the node needs. An epoch lower than one the
    /// device was attached for is refused.
    fn attach(
        &self,
        attach: pb::AttachBlockVolume,
    ) -> impl Future<Output = Result<pb::BlockAttachment, Failure>> + Send;

    /// Lets `detach.node` go of `detach.device`; nothing if it holds it not.
    fn detach(
        &self,
        detach: pb::DetachBlockVolume,
    ) -> impl Future<Output = Result<(), Failure>> + Send;

    /// Deletes `device`, data and all; nothing if it is gone already.
    fn delete(&self, device: String) -> impl Future<Output = Result<(), Failure>> + Send;

    /// What the storage says of `device`.
    fn describe(
        &self,
        device: String,
    ) -> impl Future<Output = Result<pb::BlockVolumeDescription, Failure>> + Send;
}

/// Serves a [`BlockStorage`] as a provider's [`Handler`].
#[derive(Debug, Clone)]
pub struct Storage<B>(pub B);

impl<B: BlockStorage> Handler for Storage<B> {
    async fn call(&self, call: pb::ProviderCall) -> Outcome {
        let answered = match call.request {
            Some(Request::CreateBlockVolume(c)) => self.0.create(*c).await.map(|device| {
                Outcome::BlockVolumeCreated(Box::new(pb::BlockVolumeCreated {
                    device,
                    ..Default::default()
                }))
            }),
            Some(Request::AttachBlockVolume(a)) => {
                self.0.attach(*a).await.map(|a| Outcome::BlockAttachment(Box::new(a)))
            }
            Some(Request::DetachBlockVolume(d)) => {
                self.0.detach(*d).await.map(|()| Outcome::BlockVolumeDetached(Box::default()))
            }
            Some(Request::DeleteBlockVolume(d)) => {
                self.0.delete(d.device).await.map(|()| Outcome::BlockVolumeDeleted(Box::default()))
            }
            Some(Request::DescribeBlockVolume(d)) => self
                .0
                .describe(d.device)
                .await
                .map(|d| Outcome::BlockVolumeDescription(Box::new(d))),
            _ => return Unsupported.call(call).await,
        };
        answered.unwrap_or_else(Into::into)
    }
}
