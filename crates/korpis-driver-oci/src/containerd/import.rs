// SPDX-License-Identifier: AGPL-3.0-or-later

//! Importing an image from the node's content store into containerd's. The driver reads the
//! store, never writes it, and checks every blob against its digest again: containerd commits a
//! blob only when its bytes match the digest it is given. The manifest is committed with the
//! garbage-collection labels that tie its configuration and layers to it, and an image record
//! names it, so what was imported stays until the image is removed.

use super::{Containerd, under};
use crate::error::{DriverError, DriverResult};
use crate::image::ImageConfig;
use korpis_containerd::containerd::services::content::v1 as content;
use korpis_containerd::containerd::services::images::v1 as images;
use korpis_containerd::containerd::types;
use korpis_core::content::{self as oci, Descriptor, Document};
use std::collections::BTreeMap;
use tokio::io::AsyncReadExt as _;

/// How much of a blob one write carries.
const CHUNK: usize = 1 << 20;

/// An image, imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Imported {
    /// What the image says about running it.
    pub(crate) config: ImageConfig,
    /// Its layers, bottom first.
    pub(crate) layers: Vec<Descriptor>,
    /// The digest of its configuration.
    pub(crate) config_digest: String,
}

fn failed(why: impl std::fmt::Display) -> DriverError {
    DriverError::Failed(why.to_string())
}

impl Containerd {
    /// The bytes of the blob `digest` in the content store, checked.
    async fn read_blob(&self, digest: &str) -> DriverResult<Vec<u8>> {
        let bytes = tokio::fs::read(self.blob(digest)?).await.map_err(|e| {
            DriverError::Rejected(format!("{digest} is not in the node's content store: {e}"))
        })?;
        if oci::digest_of(&bytes) != digest {
            return Err(failed(format!("the content store's {digest} does not match its digest")));
        }
        Ok(bytes)
    }

    /// Imports the image `rootfs` (`name@sha256:...`) under `lease`.
    pub(crate) async fn import(&self, lease: &str, rootfs: &str) -> DriverResult<Imported> {
        let (_, top) = rootfs.split_once('@').ok_or_else(|| failed("an image by digest"))?;
        let top_bytes = self.read_blob(top).await?;
        let mut top_doc = oci::read(&top_bytes).map_err(failed)?;
        let mut stored = vec![(top.to_owned(), top_bytes, media(&top_doc))];
        if let Document::Index(ms) = &top_doc {
            let chosen = oci::select(ms, oci::this_machine())
                .map_err(|e| DriverError::Rejected(e.to_string()))?
                .digest
                .clone();
            let bytes = self.read_blob(&chosen).await?;
            let manifest = oci::read(&bytes).map_err(failed)?;
            let index = stored.pop().ok_or_else(|| failed("no index"))?;
            stored.push((chosen, bytes, media(&manifest)));
            stored.push(index);
            top_doc = manifest;
        }
        let Document::Image { config, layers } = top_doc else {
            return Err(failed("an index names an index"));
        };
        let config_bytes = self.read_blob(&config.digest).await?;
        let image =
            ImageConfig::read(&config_bytes).map_err(|e| DriverError::Rejected(e.to_string()))?;
        if image.diff_ids.len() != layers.len() {
            return Err(failed("the configuration and the manifest disagree on the layers"));
        }
        for l in &layers {
            self.write(lease, &l.digest, l.size, BTreeMap::new()).await?;
        }
        self.write(lease, &config.digest, config.size, BTreeMap::new()).await?;
        self.commit_manifests(lease, &stored, &config, &layers).await?;
        self.record(lease, rootfs, top, &stored).await?;
        Ok(Imported { config: image, layers, config_digest: config.digest })
    }

    /// Commits the manifest, and the index if there is one, labelled with what they reference.
    async fn commit_manifests(
        &self,
        lease: &str,
        stored: &[(String, Vec<u8>, &'static str)],
        config: &Descriptor,
        layers: &[Descriptor],
    ) -> DriverResult<()> {
        let mut previous: Option<&str> = None;
        for (digest, bytes, _) in stored {
            let labels = match previous {
                None => manifest_labels(config, layers),
                Some(manifest) => BTreeMap::from([(
                    "containerd.io/gc.ref.content.m.0".into(),
                    manifest.to_owned(),
                )]),
            };
            let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            self.write(lease, digest, size, labels).await?;
            previous = Some(digest);
        }
        Ok(())
    }

    /// Names the image, so what was imported is kept.
    async fn record(
        &self,
        lease: &str,
        rootfs: &str,
        top: &str,
        stored: &[(String, Vec<u8>, &'static str)],
    ) -> DriverResult<()> {
        let (size, media_type) = stored
            .iter()
            .find(|(d, _, _)| d == top)
            .map(|(_, b, m)| (b.len(), *m))
            .ok_or_else(|| failed("the top of the image was not stored"))?;
        let image = images::Image {
            name: rootfs.to_owned(),
            target: types::Descriptor {
                media_type: media_type.to_owned(),
                digest: top.to_owned(),
                size: i64::try_from(size).unwrap_or(i64::MAX),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        let request = images::CreateImageRequest { image: image.into(), ..Default::default() };
        let client = images::ImagesClient::new(self.conn.clone(), self.config.clone());
        match client.create_with_options(request, under(lease)).await {
            Ok(_) => Ok(()),
            Err(e) if e.code == connectrpc::ErrorCode::AlreadyExists => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether containerd holds the blob `digest`.
    async fn holds(&self, digest: &str) -> DriverResult<bool> {
        let request = content::InfoRequest { digest: digest.to_owned(), ..Default::default() };
        match content::ContentClient::new(self.conn.clone(), self.config.clone())
            .info(request)
            .await
        {
            Ok(_) => Ok(true),
            Err(e) if e.code == connectrpc::ErrorCode::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes the blob `digest` from the store into containerd's, unless it holds it.
    async fn write(
        &self,
        lease: &str,
        digest: &str,
        size: u64,
        labels: BTreeMap<String, String>,
    ) -> DriverResult<()> {
        if self.holds(digest).await? {
            return Ok(());
        }
        let mut file = tokio::fs::File::open(self.blob(digest)?).await?;
        if file.metadata().await?.len() != size {
            return Err(failed(format!("{digest} in the content store is not {size} bytes")));
        }
        let client = content::ContentClient::new(self.conn.clone(), self.config.clone());
        let mut stream = client.write_with_options(under(lease)).await?;
        let reference = format!("korpis-import-{}", oci::hex_of(digest).map_err(failed)?);
        let total = i64::try_from(size).unwrap_or(i64::MAX);
        let request = |action: content::WriteAction, offset: i64, data: Vec<u8>| {
            content::WriteContentRequest {
                action: action.into(),
                r#ref: reference.clone(),
                total,
                expected: digest.to_owned(),
                offset,
                data,
                ..Default::default()
            }
        };
        let mut offset = 0i64;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let data = buf.get(..n).unwrap_or_default().to_vec();
            stream.send(request(content::WriteAction::WRITE, offset, data)).await?;
            stream.message::<content::WriteContentResponse>().await?;
            offset = offset.saturating_add(i64::try_from(n).unwrap_or(i64::MAX));
        }
        let mut commit = request(content::WriteAction::COMMIT, offset, Vec::new());
        commit.labels = labels;
        stream.send(commit).await?;
        stream.close_send();
        while stream.message::<content::WriteContentResponse>().await?.is_some() {}
        Ok(())
    }
}

/// The labels that tie a manifest's configuration and layers to it for garbage collection.
fn manifest_labels(config: &Descriptor, layers: &[Descriptor]) -> BTreeMap<String, String> {
    let mut labels =
        BTreeMap::from([("containerd.io/gc.ref.content.config".to_owned(), config.digest.clone())]);
    for (i, l) in layers.iter().enumerate() {
        labels.insert(format!("containerd.io/gc.ref.content.l.{i}"), l.digest.clone());
    }
    labels
}

/// The media type of a manifest or index.
fn media(d: &Document) -> &'static str {
    match d {
        Document::Index(_) => oci::OCI_INDEX,
        Document::Image { .. } => oci::OCI_MANIFEST,
    }
}
