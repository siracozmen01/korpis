// SPDX-License-Identifier: AGPL-3.0-or-later

//! The block storage contract (§2.1 of docs/06-storage.md, §5 of docs/16-extensions.md): an
//! extension requiring `provider.block_storage` installs, and a provider written with the SDK
//! registers saying its attach fences, which the registration shows. The storage here is the
//! tests' own: devices kept in memory, each attached to one node for one epoch, a lower epoch
//! refused.

use crate::extensions::{DNS, push_document};
use crate::providers::{Installed, installed_from};
use crate::support::Harness;
use korpis_extension::block::{BlockStorage, Failure, Storage};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::future::{Future, ready};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// One device: its size, and the node and epoch it is attached for.
#[derive(Debug, Clone, Default)]
pub(crate) struct Device {
    pub(crate) size_bytes: u64,
    pub(crate) attached: Option<(String, u64)>,
}

/// The storage, as its provider keeps it: devices by name, named for their volume.
#[derive(Default, Clone)]
pub(crate) struct Disks(pub(crate) Arc<Mutex<BTreeMap<String, Device>>>);

impl Disks {
    fn with<T>(&self, f: impl FnOnce(&mut BTreeMap<String, Device>) -> T) -> T {
        f(&mut self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

fn refused(reason: &str) -> Failure {
    Failure { reason: reason.into(), message: String::new(), retryable: false }
}

impl Disks {
    fn create_now(&self, c: &pb::CreateBlockVolume) -> String {
        let device = format!("{}.img", c.volume);
        self.with(|d| {
            d.entry(device.clone())
                .or_insert(Device { size_bytes: c.size_bytes, ..Device::default() });
        });
        device
    }

    fn attach_now(&self, a: &pb::AttachBlockVolume) -> Result<pb::BlockAttachment, Failure> {
        self.with(|d| {
            let device = d.get_mut(&a.device).ok_or_else(|| refused("block.unknown"))?;
            if device.attached.as_ref().is_some_and(|(_, e)| *e > a.epoch) {
                return Err(refused("block.epoch_superseded"));
            }
            device.attached = Some((a.node.clone(), a.epoch));
            Ok(pb::BlockAttachment {
                protocol: pb::BlockProtocol::BLOCK_PROTOCOL_LOOP.into(),
                target: a.device.clone(),
                ..Default::default()
            })
        })
    }

    fn describe_now(&self, device: &str) -> Result<pb::BlockVolumeDescription, Failure> {
        self.with(|d| {
            let found = d.get(device).ok_or_else(|| refused("block.unknown"))?;
            let (node, epoch) = found.attached.clone().unwrap_or_default();
            Ok(pb::BlockVolumeDescription {
                size_bytes: found.size_bytes,
                node,
                epoch,
                ..Default::default()
            })
        })
    }
}

impl BlockStorage for Disks {
    fn create(
        &self,
        c: pb::CreateBlockVolume,
    ) -> impl Future<Output = Result<String, Failure>> + Send {
        ready(Ok(self.create_now(&c)))
    }

    fn attach(
        &self,
        a: pb::AttachBlockVolume,
    ) -> impl Future<Output = Result<pb::BlockAttachment, Failure>> + Send {
        ready(self.attach_now(&a))
    }

    fn detach(&self, d: pb::DetachBlockVolume) -> impl Future<Output = Result<(), Failure>> + Send {
        self.with(|disks| {
            if let Some(device) = disks.get_mut(&d.device)
                && device.attached.as_ref().is_some_and(|(n, _)| *n == d.node)
            {
                device.attached = None;
            }
        });
        ready(Ok(()))
    }

    fn delete(&self, device: String) -> impl Future<Output = Result<(), Failure>> + Send {
        self.with(|d| d.remove(&device));
        ready(Ok(()))
    }

    fn describe(
        &self,
        device: String,
    ) -> impl Future<Output = Result<pb::BlockVolumeDescription, Failure>> + Send {
        ready(self.describe_now(&device))
    }
}

/// A block storage provider named `fast`, installed in the root organization and serving
/// `disks`, its attach fencing; the installation, whose node's session is kept for as long as
/// it is.
pub(crate) async fn serving(h: &Harness, disks: Disks) -> Installed {
    let document = DNS
        .replace("    - {type: dns, name: cloudflare}", "    - {type: block_storage, name: fast}")
        .replace(
            "requires: [provider.serve, provider.dns,",
            "requires: [provider.serve, provider.block_storage,",
        );
    let x = installed_from(h, 82, "disks", push_document(&document).await).await;
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_BLOCK_STORAGE,
        name: "fast".into(),
        version: "1.0.0".into(),
        concurrency: 4,
        sign_in: false,
        fencing: pb::Fencing::FENCING_STORAGE_ENFORCED,
    };
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let provider = Arc::new(Provider::new(connection, registration, Storage(disks)));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
    x
}

#[tokio::test]
async fn a_block_storage_provider_installs_and_registers_saying_its_attach_fences() {
    let h = Harness::new().await;
    let x = serving(&h, Disks::default()).await;
    let listed = pb::ProviderServiceClient::new(h.http.clone(), h.root())
        .list_providers(pb::ListProvidersRequest {
            organization: h.root_org(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let [p] = listed.providers.as_slice() else { panic!("one: {listed:?}") };
    assert_eq!(p.extension, x.extension.id);
    assert_eq!(p.r#type.as_known(), Some(pb::ProviderType::PROVIDER_TYPE_BLOCK_STORAGE));
    assert_eq!(p.fencing.as_known(), Some(pb::Fencing::FENCING_STORAGE_ENFORCED));
}
