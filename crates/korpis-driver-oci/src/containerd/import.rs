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

    /// Writes the blob `digest` from the store into containerd's, unless it holds it. As
    /// containerd's own client does, a write refused because another holds the same ingest ref,
    /// importing the same content, is asked again until that one is done, and a commit answered
    /// that the content already exists is a write: two workloads of one image placed at once
    /// both import it, and neither may fail for the other.
    async fn write(
        &self,
        lease: &str,
        digest: &str,
        size: u64,
        labels: BTreeMap<String, String>,
    ) -> DriverResult<()> {
        let started = tokio::time::Instant::now();
        let mut wait = LOCKED_FIRST;
        loop {
            if self.holds(digest).await? {
                return Ok(());
            }
            match self.write_once(lease, digest, size, &labels).await {
                Ok(()) | Err(Writing::Exists) => return Ok(()),
                Err(Writing::Locked(why)) if started.elapsed() < LOCKED_AT_MOST => {
                    tracing::debug!(digest, why, "another import holds the blob; waiting");
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(LOCKED_MOST);
                }
                Err(Writing::Locked(why)) => return Err(DriverError::Unavailable(why)),
                Err(Writing::Other(e)) => return Err(e),
            }
        }
    }

    /// One write of the blob `digest`, under the ingest ref its digest names.
    async fn write_once(
        &self,
        lease: &str,
        digest: &str,
        size: u64,
        labels: &BTreeMap<String, String>,
    ) -> Result<(), Writing> {
        let mut file = tokio::fs::File::open(self.blob(digest)?).await?;
        if file.metadata().await?.len() != size {
            return Err(failed(format!("{digest} in the content store is not {size} bytes")).into());
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
        commit.labels.clone_from(labels);
        stream.send(commit).await?;
        stream.close_send();
        while stream.message::<content::WriteContentResponse>().await?.is_some() {}
        Ok(())
    }
}

/// How long a write waits first for another import of the same blob to finish, and at most
/// between askings.
const LOCKED_FIRST: std::time::Duration = std::time::Duration::from_millis(100);
const LOCKED_MOST: std::time::Duration = std::time::Duration::from_secs(2);
/// How long a write waits for another import in all before it is the driver's failure: longer
/// than any one blob takes to write on a node that can run what it is given.
const LOCKED_AT_MOST: std::time::Duration = std::time::Duration::from_secs(600);

/// How one write of a blob ended, when it did not.
enum Writing {
    /// containerd answered `Unavailable`, which its content service says of an ingest ref
    /// another writer holds.
    Locked(String),
    /// The commit found the content already there.
    Exists,
    /// Anything else.
    Other(DriverError),
}

impl From<connectrpc::ConnectError> for Writing {
    fn from(e: connectrpc::ConnectError) -> Self {
        match e.code {
            connectrpc::ErrorCode::Unavailable => {
                Self::Locked(e.message.unwrap_or_else(|| "locked".into()))
            }
            connectrpc::ErrorCode::AlreadyExists => Self::Exists,
            _ => Self::Other(e.into()),
        }
    }
}

impl From<DriverError> for Writing {
    fn from(e: DriverError) -> Self {
        Self::Other(e)
    }
}

impl From<std::io::Error> for Writing {
    fn from(e: std::io::Error) -> Self {
        Self::Other(e.into())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Only where containerd runs and this process may reach it, which takes root.
    const SOCKET: &str = "/run/containerd/containerd.sock";

    #[tokio::test]
    async fn two_imports_of_one_blob_at_once_both_succeed() {
        if std::os::unix::net::UnixStream::connect(SOCKET).is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // A blob nobody has imported before, large enough that two writes of it overlap.
        let mut bytes = vec![0u8; 3 * CHUNK];
        let nonce = uuid::Uuid::now_v7();
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = nonce.as_bytes()[i % 16] ^ u8::try_from(i % 251).unwrap();
        }
        let digest = oci::digest_of(&bytes);
        let blobs = dir.path().join("blobs/sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(blobs.join(oci::hex_of(&digest).unwrap()), &bytes).unwrap();
        let c = Containerd::connect(Path::new(SOCKET), dir.path()).await.unwrap();
        let lease = c.lease().await.unwrap();
        let size = u64::try_from(bytes.len()).unwrap();
        let (a, b) = tokio::join!(
            c.write(&lease, &digest, size, BTreeMap::new()),
            c.write(&lease, &digest, size, BTreeMap::new()),
        );
        let held = c.holds(&digest).await;
        c.release(&lease).await;
        a.unwrap();
        b.unwrap();
        assert!(held.unwrap(), "containerd holds the blob once, under the lease");
    }
}
