// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's content store (§6 of docs/04-runtimes.md): the agent, not a driver, fetches images,
//! once per digest per node, verifies every byte against the digest that names it, and keeps
//! them as files named by digest. Drivers reference content by digest and import it from here;
//! they hold no registry credentials and no path to a registry.
//!
//! A blob is written to an ingest file that refuses bytes beyond the size its descriptor
//! declares, hashed as it is written, synced, and only then renamed into place, so a file under
//! `blobs/` is always whole and always what its name says.

use korpis_core::content::{self, ContentError, Descriptor, Document};
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::manifest::OciDescriptor;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::AsyncWrite;

/// The manifest and index types a node reads.
const ACCEPTED: [&str; 4] =
    [content::OCI_MANIFEST, content::OCI_INDEX, content::DOCKER_MANIFEST, content::DOCKER_LIST];

/// Why content could not be fetched.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The reference is malformed, or names no digest: content is fetched by digest only.
    #[error("{0}")]
    Reference(String),
    /// The registry refused or could not be reached.
    #[error("the registry: {0}")]
    Registry(String),
    /// What arrived is not what the digest names, or not an image.
    #[error(transparent)]
    Content(#[from] ContentError),
    /// The bytes are not the bytes the digest names.
    #[error("{0} does not match its digest")]
    Mismatch(String),
    /// The URL is at an address a fetch may not reach (§4 of docs/09-recipes.md).
    #[error("refused: {0}")]
    Refused(String),
    /// The store could not be written.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// An ingest file that refuses bytes past the declared size and hashes what it takes.
struct Capped {
    file: tokio::fs::File,
    left: u64,
    hash: Sha256,
}

impl AsyncWrite for Capped {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if len > self.left {
            let e = std::io::Error::other("the blob is larger than its descriptor declares");
            return Poll::Ready(Err(e));
        }
        let this = &mut *self;
        let written = Pin::new(&mut this.file).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &written {
            this.hash.update(buf.get(..*n).unwrap_or_default());
            this.left = this.left.saturating_sub(u64::try_from(*n).unwrap_or(u64::MAX));
        }
        written
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.file).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.file).poll_shutdown(cx)
    }
}

pub mod reach;
mod url;

pub use reach::Reach;

/// The content store.
pub struct ContentStore {
    root: PathBuf,
    client: Client,
    auth: RegistryAuth,
    /// For an install's `fetch` (§4 of docs/09-recipes.md), and where it may go.
    http: reqwest::Client,
    reach: Reach,
}

impl std::fmt::Debug for ContentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentStore").field("root", &self.root).finish_non_exhaustive()
    }
}

fn hex_digest(hash: Sha256) -> String {
    let bytes: [u8; 32] = hash.finalize().into();
    bytes.iter().fold(String::from("sha256:"), |mut s, b| {
        use std::fmt::Write as _;
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
        s
    })
}

impl ContentStore {
    /// The store under `root`, fetching over HTTPS from every registry but those named in
    /// `plain_http`, which are development registries on loopback.
    pub fn open(root: &Path, plain_http: Vec<String>) -> std::io::Result<Self> {
        std::fs::create_dir_all(root.join("blobs/sha256"))?;
        std::fs::create_dir_all(root.join("blobs/sha512"))?;
        std::fs::create_dir_all(root.join("ingest"))?;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let protocol = if plain_http.is_empty() {
            ClientProtocol::Https
        } else {
            ClientProtocol::HttpsExcept(plain_http)
        };
        let client = Client::new(ClientConfig { protocol, ..ClientConfig::default() });
        let reach = Reach::default();
        let http = url::client(&reach).map_err(std::io::Error::other)?;
        Ok(Self { root: root.to_owned(), client, auth: RegistryAuth::Anonymous, http, reach })
    }

    /// The store, with an install's `fetch` reaching `reach`: the public internet, and the
    /// networks the operator allows.
    ///
    /// # Errors
    /// The client cannot be built.
    pub fn reaching(mut self, reach: Reach) -> std::io::Result<Self> {
        self.http = url::client(&reach).map_err(std::io::Error::other)?;
        self.reach = reach;
        Ok(self)
    }

    /// Where the blob `digest` is, or would be.
    pub fn path_of(&self, digest: &str) -> Result<PathBuf, ContentError> {
        Ok(self.root.join("blobs/sha256").join(content::hex_of(digest)?))
    }

    /// Whether the blob of `d` is held, whole.
    fn holds(&self, d: &Descriptor) -> bool {
        self.path_of(&d.digest)
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .is_some_and(|m| m.len() == d.size)
    }

    /// Fetches the artifact `reference` names (`name@sha256:...`), a recipe's (§2 of
    /// docs/09-recipes.md), with every layer; each titled layer's title and digest, which is how
    /// an install finds a recipe's files.
    pub async fn fetch_files(
        &self,
        reference: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, FetchError> {
        let r: Reference =
            reference.parse().map_err(|e| FetchError::Reference(format!("{reference:?}: {e}")))?;
        let Some(digest) = r.digest().map(str::to_owned) else {
            return Err(FetchError::Reference(format!("{reference:?} names no digest")));
        };
        let Document::Image { layers, .. } = self.manifest(&r, &digest).await? else {
            return Err(ContentError::Format("an artifact is an image manifest".into()).into());
        };
        let mut files = std::collections::BTreeMap::new();
        for d in &layers {
            self.blob(&r, d).await?;
            if let Some(title) = d.title() {
                files.insert(title.to_owned(), d.digest.clone());
            }
        }
        Ok(files)
    }

    /// Fetches the image `reference` names (`name@sha256:...`) with every blob it needs for this
    /// machine; the digest of the image manifest chosen.
    pub async fn fetch(&self, reference: &str) -> Result<String, FetchError> {
        let r: Reference =
            reference.parse().map_err(|e| FetchError::Reference(format!("{reference:?}: {e}")))?;
        let Some(digest) = r.digest().map(str::to_owned) else {
            return Err(FetchError::Reference(format!("{reference:?} names no digest")));
        };
        let mut chosen = digest;
        let mut doc = self.manifest(&r, &chosen).await?;
        if let Document::Index(manifests) = &doc {
            chosen = content::select(manifests, content::this_machine())?.digest.clone();
            doc = self.manifest(&r, &chosen).await?;
        }
        let Document::Image { config, layers } = doc else {
            return Err(ContentError::Format("an index names an index".into()).into());
        };
        for d in std::iter::once(&config).chain(layers.iter()) {
            self.blob(&r, d).await?;
        }
        Ok(chosen)
    }

    /// The manifest or index `digest`, held or fetched, checked against its digest.
    async fn manifest(&self, r: &Reference, digest: &str) -> Result<Document, FetchError> {
        let path = self.path_of(digest)?;
        if let Ok(bytes) = tokio::fs::read(&path).await
            && content::digest_of(&bytes) == digest
        {
            return Ok(content::read(&bytes)?);
        }
        let (raw, _) = self
            .client
            .pull_manifest_raw(&r.clone_with_digest(digest.to_owned()), &self.auth, &ACCEPTED)
            .await
            .map_err(|e| FetchError::Registry(e.to_string()))?;
        if content::digest_of(&raw) != digest {
            return Err(FetchError::Mismatch(digest.to_owned()));
        }
        let doc = content::read(&raw)?;
        self.keep(digest, &raw).await?;
        Ok(doc)
    }

    /// Writes `bytes`, already checked against `digest`, into place.
    async fn keep(&self, digest: &str, bytes: &[u8]) -> Result<(), FetchError> {
        let ingest = self.root.join("ingest").join(content::hex_of(digest)?);
        tokio::fs::write(&ingest, bytes).await?;
        self.commit(&ingest, digest).await
    }

    /// Syncs an ingest file and renames it into place.
    async fn commit(&self, ingest: &Path, digest: &str) -> Result<(), FetchError> {
        tokio::fs::File::open(ingest).await?.sync_all().await?;
        tokio::fs::rename(ingest, self.path_of(digest)?).await?;
        crate::durable::sync_dir(&self.root.join("blobs/sha256"))?;
        Ok(())
    }

    /// The blob `d` names, held or fetched.
    async fn blob(&self, r: &Reference, d: &Descriptor) -> Result<(), FetchError> {
        if self.holds(d) {
            return Ok(());
        }
        let ingest = self.root.join("ingest").join(content::hex_of(&d.digest)?);
        let file = tokio::fs::File::create(&ingest).await?;
        let mut out = Capped { file, left: d.size, hash: Sha256::new() };
        let descriptor = OciDescriptor {
            media_type: d.media_type.clone(),
            digest: d.digest.clone(),
            size: i64::try_from(d.size).unwrap_or(i64::MAX),
            ..OciDescriptor::default()
        };
        let pulled = self.client.pull_blob(r, &descriptor, &mut out).await;
        if let Err(e) = pulled {
            let _ = tokio::fs::remove_file(&ingest).await;
            return Err(FetchError::Registry(e.to_string()));
        }
        if out.left != 0 || hex_digest(out.hash) != d.digest {
            let _ = tokio::fs::remove_file(&ingest).await;
            return Err(FetchError::Mismatch(d.digest.clone()));
        }
        self.commit(&ingest, &d.digest).await
    }
}
