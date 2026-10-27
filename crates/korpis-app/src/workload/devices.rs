// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class's devices (§2.1 of docs/06-storage.md). Each pass, every workload placed on a
//! node offering a block class has a device wanted for each volume it declares in such a class,
//! kept by the class's store and the identifier the volume has there. Each device due is then
//! taken under a hold, so control planes share the work, and its class's provider, the
//! `block_storage` provider of the class's name that the workload's organization sees, is asked
//! to make it, and once made to attach it to the node its workload holds its lease on, for that
//! lease's epoch, the credential it answers sealed to the device; to detach it once no lease is
//! held for the node and epoch it was attached for; and to delete it once it is gone, its volume
//! deleted. A failure is kept with its reason and asked again on a backoff.

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::page::Page;
use crate::ports::{BlockWrite as _, Clock, RecipeSource, SealedAttachment, Store, Tx as _};
use korpis_core::extension::block::{Attachment, BlockAttach, BlockCreate};
use korpis_core::extension::{CallOutcome, CallRequest, ProviderType};
use korpis_core::id::{Id, IdKind};
use korpis_core::node::StorageClass;
use korpis_core::workload::block::{DeviceCall, DueDevice, WantedDevice, device_backoff};
use korpis_core::workload::spec::VolumeSpec;
use std::time::Duration;
use uuid::Uuid;

/// How long a control plane holds the device it took.
const HOLD: Duration = Duration::from_secs(5 * 60);
/// How long a provider is given to answer.
const CALL_WITHIN: Duration = Duration::from_secs(60);

/// The class `spec` is in among `classes`: the one it names, or the node's first.
fn class_of<'a>(
    spec: &VolumeSpec,
    name: &str,
    classes: &'a [StorageClass],
) -> Option<&'a StorageClass> {
    let name = if name.is_empty() { spec.storage_class.as_str() } else { name };
    if name.is_empty() { classes.first() } else { classes.iter().find(|c| c.name == name) }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// One pass making a device wanted for each volume of a block class that a placed workload
    /// declares; how many were new.
    pub async fn tend_devices(&self) -> Result<u64, AppError> {
        let (mut page, mut wanted) = (Page::new(None, Page::DEFAULT), 0);
        loop {
            let placed = self.store.placed_on_block(page).await?;
            for (workload, node) in &placed {
                wanted += self.want_devices(*workload, *node).await?;
            }
            match page.next(&placed, |(w, _)| *w) {
                Some(after) => page = Page::new(Some(after), page.size),
                None => return Ok(wanted),
            }
        }
    }

    /// Wants a device for each volume `workload` declares in a block class of `node`; how many
    /// were new.
    async fn want_devices(&self, workload: Uuid, node: Uuid) -> Result<u64, AppError> {
        let Some(w) = self.store.workload(workload).await? else { return Ok(0) };
        let Some(target) = w.target else { return Ok(0) };
        let Some(intent) = self.store.intent(target).await? else { return Ok(0) };
        let classes = self.store.node(node).await?.map(|n| n.storage_classes).unwrap_or_default();
        let mut wanted = Vec::new();
        for spec in &intent.body.volumes {
            let existing = match spec.existing {
                Some(id) => self.store.volume(id).await?,
                None => None,
            };
            let named = existing.as_ref().map_or("", |v| v.storage_class.as_str());
            let Some(class) = class_of(spec, named, &classes) else { continue };
            let (Some(_), Some(store)) = (class.block, class.store.clone()) else { continue };
            let derived = || format!("{}.{}", Id::from_uuid(IdKind::Workload, workload), spec.name);
            wanted.push(WantedDevice {
                id: existing.as_ref().map_or_else(Uuid::now_v7, |v| v.id),
                organization: w.organization,
                workload,
                store,
                helper_id: existing.as_ref().map_or_else(derived, |v| v.helper_id.clone()),
                name: spec.name.clone(),
                storage_class: class.name.clone(),
                size_bytes: existing.as_ref().map_or(spec.size_bytes, |v| v.size_bytes),
            });
        }
        if wanted.is_empty() {
            return Ok(0);
        }
        let mut tx = self.store.begin().await?;
        let mut new = 0;
        for d in &wanted {
            new += u64::from(tx.want_device(d).await?);
        }
        tx.commit().await?;
        Ok(new)
    }

    /// Asks the provider of the next device due, if one is; whether one was.
    pub async fn call_next_device(&self) -> Result<bool, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let due = tx.take_due_device(now, now + HOLD).await?;
        tx.commit().await?;
        let Some(due) = due else { return Ok(false) };
        let asked = self.ask_storage(&due).await;
        let mut tx = self.store.begin().await?;
        match asked {
            Ok(Answered::Created(device)) => tx.device_created(due.id, &device).await?,
            Ok(Answered::Attached(at, a)) => tx.device_attached(due.id, at, &a).await?,
            Ok(Answered::Detached) => tx.device_detached(due.id).await?,
            Ok(Answered::Deleted) => tx.device_deleted(due.id, self.clock.now()).await?,
            Ok(Answered::Nothing) => {}
            Ok(Answered::Failed(why)) => self.failed(&mut tx, &due, &why).await?,
            Err(e) => self.failed(&mut tx, &due, &e.to_string()).await?,
        }
        tx.commit().await?;
        Ok(true)
    }

    async fn failed(&self, tx: &mut S::Tx, due: &DueDevice, why: &str) -> Result<(), AppError> {
        let attempts = due.attempts.saturating_add(1);
        let again = self.clock.now() + device_backoff(attempts);
        tx.device_failed(due.id, (why, attempts), again).await?;
        Ok(())
    }

    /// Asks `due`'s provider for what it is due; a device never made is forgotten unasked.
    async fn ask_storage(&self, due: &DueDevice) -> Result<Answered, AppError> {
        let request = match due.next() {
            None => return Ok(Answered::Nothing),
            Some(DeviceCall::Forget) => return Ok(Answered::Deleted),
            Some(call) => match request(due, call) {
                Some(r) => r,
                None => return Ok(Answered::Nothing),
            },
        };
        let target = CallTarget {
            organization: due.organization,
            kind: ProviderType::BlockStorage,
            name: due.storage_class.clone(),
        };
        let called = self.call_provider(&target, request.clone(), CALL_WITHIN).await?;
        let provider = &due.storage_class;
        Ok(match (called.outcome, &request) {
            (CallOutcome::BlockCreated { device }, _) => Answered::Created(device),
            (CallOutcome::BlockAttached(a), CallRequest::AttachBlockVolume(asked)) => {
                Answered::Attached((asked.node, asked.epoch), self.sealed(due.id, a)?)
            }
            (CallOutcome::BlockDetached, _) => Answered::Detached,
            (CallOutcome::BlockDeleted, _) => Answered::Deleted,
            (CallOutcome::Failed(f), _) => {
                Answered::Failed(format!("provider {provider}: {}: {}", f.reason, f.message))
            }
            _ => Answered::Failed(format!("provider {provider}: an answer that answers nothing")),
        })
    }

    /// The devices of `workload`'s volumes attached to `node` for `epoch`, each by the helper's
    /// identifier of its volume, their credentials opened for the node's assignment, which is
    /// sealed to the node alone (§2.1 of docs/06-storage.md).
    pub(crate) async fn attached_on(
        &self,
        workload: Uuid,
        (node, epoch): (Uuid, u64),
    ) -> Result<Vec<(String, Attachment)>, AppError> {
        if epoch == 0 {
            return Ok(Vec::new());
        }
        let held = self.store.held_devices(workload, (node, epoch)).await?;
        let mut out = Vec::with_capacity(held.len());
        for d in held {
            let credential = match &d.attachment.credential {
                None => String::new(),
                Some(sealed) => {
                    let vault =
                        self.device_vault.as_ref().ok_or_else(|| AppError::Unavailable {
                            what: "block storage",
                            detail: "no device vault is configured".into(),
                        })?;
                    let opened = vault.open(d.id, sealed);
                    opened.map_err(|e| AppError::Internal(e.to_string()))?.to_string()
                }
            };
            let SealedAttachment { protocol, target, .. } = d.attachment;
            out.push((d.helper_id, Attachment { protocol, target, credential }));
        }
        Ok(out)
    }

    /// `a`, its credential sealed to `device`.
    fn sealed(&self, device: Uuid, a: Attachment) -> Result<SealedAttachment, AppError> {
        let credential = if a.credential.is_empty() {
            None
        } else {
            let vault = self.device_vault.as_ref().ok_or_else(|| AppError::Unavailable {
                what: "block storage",
                detail: "no device vault is configured".into(),
            })?;
            Some(vault.seal(device, &a.credential).map_err(|e| AppError::Internal(e.to_string()))?)
        };
        Ok(SealedAttachment { protocol: a.protocol, target: a.target, credential })
    }
}

/// How a provider answered.
enum Answered {
    /// It made the device, by this name.
    Created(String),
    /// It attached it to this node for this epoch, answering this.
    Attached((Uuid, u64), SealedAttachment),
    /// It detached it.
    Detached,
    /// It deleted it, or it was never made.
    Deleted,
    /// Nothing was due after all.
    Nothing,
    /// It failed, saying why.
    Failed(String),
}

/// The call asking for `call` of `due`; none to forget it, which no provider is asked.
fn request(due: &DueDevice, call: DeviceCall) -> Option<CallRequest> {
    Some(match call {
        DeviceCall::Forget => return None,
        DeviceCall::Create => CallRequest::CreateBlockVolume(BlockCreate {
            volume: due.id,
            storage_class: due.storage_class.clone(),
            size_bytes: due.size_bytes,
        }),
        DeviceCall::Attach { device, node, epoch, protocol } => {
            CallRequest::AttachBlockVolume(BlockAttach {
                device,
                node,
                epoch,
                protocols: vec![protocol],
            })
        }
        DeviceCall::Detach { device, node, .. } => CallRequest::DetachBlockVolume { device, node },
        DeviceCall::Delete { device } => CallRequest::DeleteBlockVolume { device },
    })
}
