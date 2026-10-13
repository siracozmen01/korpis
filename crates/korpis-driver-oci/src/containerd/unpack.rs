// SPDX-License-Identifier: AGPL-3.0-or-later

//! Unpacking an imported image into snapshots, one committed snapshot per layer, each named by
//! its chain identifier, so two images sharing a base share its snapshots. containerd's diff
//! service applies each layer, decompressing it, and the driver checks that what it applied is
//! the uncompressed layer the image configuration names before committing it.

use super::import::Imported;
use super::{Containerd, SNAPSHOTTER, under};
use crate::error::{DriverError, DriverResult};
use buffa_types::google::protobuf::FieldMask;
use korpis_containerd::containerd::services::content::v1 as content;
use korpis_containerd::containerd::services::diff::v1 as diff;
use korpis_containerd::containerd::services::snapshots::v1 as snapshots;
use korpis_containerd::containerd::types;
use korpis_core::content::{Descriptor, chain_ids, hex_of};

/// The label that keeps an image's unpacked snapshots for as long as its configuration is kept.
const SNAPSHOT_REF: &str = "containerd.io/gc.ref.snapshot.overlayfs";

fn failed(why: impl std::fmt::Display) -> DriverError {
    DriverError::Failed(why.to_string())
}

impl Containerd {
    fn snapshots(&self) -> snapshots::SnapshotsClient<super::Transport> {
        snapshots::SnapshotsClient::new(self.conn.clone(), self.config.clone())
    }

    /// Whether the snapshot `key` exists.
    pub(crate) async fn snapshot_exists(&self, key: &str) -> DriverResult<bool> {
        let request = snapshots::StatSnapshotRequest {
            snapshotter: SNAPSHOTTER.into(),
            key: key.to_owned(),
            ..Default::default()
        };
        match self.snapshots().stat(request).await {
            Ok(_) => Ok(true),
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Unpacks `image` under `lease`; the chain identifier of its top layer.
    pub(crate) async fn unpack(&self, lease: &str, image: &Imported) -> DriverResult<String> {
        let chains = chain_ids(&image.config.diff_ids);
        let mut parent = String::new();
        for ((layer, chain), diff_id) in
            image.layers.iter().zip(&chains).zip(&image.config.diff_ids)
        {
            if !self.snapshot_exists(chain).await? {
                self.apply(lease, layer, &parent, chain, diff_id).await?;
            }
            parent.clone_from(chain);
        }
        if parent.is_empty() {
            return Err(DriverError::Rejected("the image has no layers".into()));
        }
        self.keep_snapshots(lease, &image.config_digest, &parent).await?;
        Ok(parent)
    }

    /// Applies `layer` on top of `parent` and commits it as `chain`.
    async fn apply(
        &self,
        lease: &str,
        layer: &Descriptor,
        parent: &str,
        chain: &str,
        diff_id: &str,
    ) -> DriverResult<()> {
        let key = format!("korpis-extract-{}", hex_of(chain).map_err(failed)?);
        let prepare = snapshots::PrepareSnapshotRequest {
            snapshotter: SNAPSHOTTER.into(),
            key: key.clone(),
            parent: parent.to_owned(),
            ..Default::default()
        };
        let mounts = match self.snapshots().prepare_with_options(prepare, under(lease)).await {
            Ok(r) => r.into_owned().mounts,
            Err(e) if e.code == connectrpc::ErrorCode::AlreadyExists => {
                // A half-done extraction from before: start it again.
                self.remove_snapshot(&key).await?;
                return Box::pin(self.apply(lease, layer, parent, chain, diff_id)).await;
            }
            Err(e) => return Err(e.into()),
        };
        let request = diff::ApplyRequest {
            diff: types::Descriptor {
                media_type: layer.media_type.clone(),
                digest: layer.digest.clone(),
                size: i64::try_from(layer.size).unwrap_or(i64::MAX),
                ..Default::default()
            }
            .into(),
            mounts,
            sync_fs: true,
            ..Default::default()
        };
        let client = diff::DiffClient::new(self.conn.clone(), self.config.clone());
        let applied = client.apply_with_options(request, under(lease)).await;
        let applied = match applied {
            Ok(r) => r.into_owned().applied.as_option().map(|d| d.digest.clone()),
            Err(e) => {
                self.remove_snapshot(&key).await?;
                return Err(e.into());
            }
        };
        if applied.as_deref() != Some(diff_id) {
            self.remove_snapshot(&key).await?;
            return Err(failed(format!(
                "layer {} unpacked to {applied:?}, not {diff_id}",
                layer.digest
            )));
        }
        self.commit(lease, &key, chain).await
    }

    async fn commit(&self, lease: &str, key: &str, chain: &str) -> DriverResult<()> {
        let request = snapshots::CommitSnapshotRequest {
            snapshotter: SNAPSHOTTER.into(),
            name: chain.to_owned(),
            key: key.to_owned(),
            ..Default::default()
        };
        match self.snapshots().commit_with_options(request, under(lease)).await {
            Ok(_) => Ok(()),
            Err(e) if e.code == connectrpc::ErrorCode::AlreadyExists => {
                self.remove_snapshot(key).await
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Removes the snapshot `key`, if it is there.
    pub(crate) async fn remove_snapshot(&self, key: &str) -> DriverResult<()> {
        let request = snapshots::RemoveSnapshotRequest {
            snapshotter: SNAPSHOTTER.into(),
            key: key.to_owned(),
            ..Default::default()
        };
        match self.snapshots().remove(request).await {
            Ok(_) => Ok(()),
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Labels the image configuration with its top snapshot, so the unpacked layers are kept
    /// for as long as the image is.
    async fn keep_snapshots(&self, lease: &str, config: &str, top: &str) -> DriverResult<()> {
        let info = content::Info {
            digest: config.to_owned(),
            labels: [(SNAPSHOT_REF.to_owned(), top.to_owned())].into(),
            ..Default::default()
        };
        let request = content::UpdateRequest {
            info: info.into(),
            update_mask: FieldMask {
                paths: vec![format!("labels.{SNAPSHOT_REF}")],
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        let client = content::ContentClient::new(self.conn.clone(), self.config.clone());
        client.update_with_options(request, under(lease)).await?;
        Ok(())
    }
}
