// SPDX-License-Identifier: AGPL-3.0-or-later

//! An install's `fetch` (§4 of docs/09-recipes.md): content named by a URL and a hash, fetched by
//! the content store once per node and kept by its hash, so a second workload installing the same
//! file fetches nothing (Rule K-15), and the worker that writes tenant paths never touches the
//! network. The hash, not the transport, is what is trusted, so `http` is allowed: what arrives is
//! kept only once it hashes to what the recipe declared, and anything else is deleted unkept.

use super::{ContentStore, FetchError};
use sha2::{Digest as _, Sha256, Sha512};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;

/// The most one fetch takes, in bytes: enough for a game's whole content, and a bound on a server
/// that would send forever. The hash decides whether what arrived is kept; this decides how much
/// may arrive.
pub const MAX_FETCH: u64 = 256 << 30;

/// The client fetches are made with: no proxy from the environment, a bounded wait for the first
/// byte and between bytes, and at most ten redirects.
pub(super) fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| e.to_string())
}

/// A hash being computed, by the algorithm a digest names.
enum Hashing {
    Sha256(Sha256),
    Sha512(Sha512),
}

impl Hashing {
    fn of(digest: &str) -> Option<(Self, &'static str, &str)> {
        let (alg, hex) = digest.split_once(':')?;
        let (hashing, alg, len) = match alg {
            "sha256" => (Self::Sha256(Sha256::new()), "sha256", 64),
            "sha512" => (Self::Sha512(Sha512::new()), "sha512", 128),
            _ => return None,
        };
        let lower = hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        (hex.len() == len && lower).then_some((hashing, alg, hex))
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(h) => h.update(bytes),
            Self::Sha512(h) => h.update(bytes),
        }
    }

    fn hex(self) -> String {
        let bytes: Vec<u8> = match self {
            Self::Sha256(h) => h.finalize().to_vec(),
            Self::Sha512(h) => h.finalize().to_vec(),
        };
        bytes.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
    }
}

fn not_a_digest(digest: &str) -> FetchError {
    FetchError::Reference(format!("{digest:?} is no digest"))
}

impl ContentStore {
    /// Where the content `digest` (`sha256:` or `sha512:` and lowercase hex) is, or would be.
    ///
    /// # Errors
    /// The digest is not one.
    pub fn file_of(&self, digest: &str) -> Result<PathBuf, FetchError> {
        let (_, alg, hex) = Hashing::of(digest).ok_or_else(|| not_a_digest(digest))?;
        Ok(self.root.join("blobs").join(alg).join(hex))
    }

    /// The content `url` serves that hashes to `digest`, fetched unless it is held; where it is.
    ///
    /// # Errors
    /// The URL is not `http` or `https`, the server failed or sent more than [`MAX_FETCH`], or
    /// what it sent is not what the digest names.
    pub async fn fetch_url(&self, url: &str, digest: &str) -> Result<PathBuf, FetchError> {
        let path = self.file_of(digest)?;
        if tokio::fs::try_exists(&path).await? {
            return Ok(path);
        }
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(FetchError::Reference(format!("{url:?} is not an http or https URL")));
        }
        let (mut hashing, alg, hex) = Hashing::of(digest).ok_or_else(|| not_a_digest(digest))?;
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|e| std::io::Error::other(e.to_string()))?;
        let ingest = self.root.join("ingest").join(format!("{hex}.{}", u64::from_le_bytes(nonce)));
        let got = self.download(url, &ingest, &mut hashing).await;
        if let Err(e) = got {
            let _ = tokio::fs::remove_file(&ingest).await;
            return Err(e);
        }
        if hashing.hex() != hex {
            let _ = tokio::fs::remove_file(&ingest).await;
            return Err(FetchError::Mismatch(format!("{url} for {digest}")));
        }
        tokio::fs::File::open(&ingest).await?.sync_all().await?;
        tokio::fs::rename(&ingest, &path).await?;
        crate::durable::sync_dir(&self.root.join("blobs").join(alg))?;
        Ok(path)
    }

    /// Streams `url` into `ingest`, hashing it.
    async fn download(
        &self,
        url: &str,
        ingest: &std::path::Path,
        hashing: &mut Hashing,
    ) -> Result<(), FetchError> {
        let failed = |e: reqwest::Error| FetchError::Registry(format!("{url}: {e}"));
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(failed)?;
        let mut file = tokio::fs::File::create(ingest).await?;
        let mut taken: u64 = 0;
        while let Some(chunk) = response.chunk().await.map_err(failed)? {
            taken = taken.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if taken > MAX_FETCH {
                return Err(FetchError::Registry(format!("{url} sent over {MAX_FETCH} bytes")));
            }
            hashing.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(())
    }
}
