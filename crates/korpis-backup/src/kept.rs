// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipe content kept once per repository (§5.2 of docs/06-storage.md), beneath its `content/`
//! prefix: each content as parts of at most [`PART`] bytes, `<sha256>/<n>`, and an index,
//! `<sha256>/index`, written last, so content whose index exists is whole. A part is read and
//! written one at a time, so keeping or reading content of any size holds one part in memory.
//!
//! Recipe content is public, so it is kept as it is, unsealed; its name is its sha256, and a
//! reader checks what it reads against that name and refuses what does not match, so a store, or
//! anyone who can write it, can make content unavailable but never make it wrong.

use crate::apply::ContentSource;
use crate::error::{Error, Result};
use crate::store::ObjectStore;
use sha2::{Digest as _, Sha256};
use std::fs::File;
use std::io::{Seek as _, Write as _};
use std::os::unix::fs::FileExt as _;
use std::path::PathBuf;
use std::sync::Arc;

/// The most bytes one part holds.
pub const PART: usize = 8 << 20;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Whether `s` names content: 64 lowercase hex digits.
fn sha256_name(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// An I/O failure on content `sha256`.
fn io(sha256: &str) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io { path: format!("content {sha256}"), source }
}

/// Reads from `file` at `at` until `buf` is full or the file ends; how much was read.
fn fill(file: &File, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..).filter(|r| !r.is_empty()) {
        match file.read_at(rest, at + crate::meta::wide(filled))? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn index(sha256: &str) -> String {
    format!("{sha256}/index")
}

fn part(sha256: &str, n: u64) -> String {
    format!("{sha256}/{n}")
}

/// The repository's recipe content, in `store`, which is beneath its `content/` prefix.
#[derive(Debug)]
pub struct Kept<S> {
    store: Arc<S>,
}

impl<S: ObjectStore> Kept<S> {
    /// The content kept in `store`.
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    /// Whether content `sha256` is kept whole.
    pub async fn holds(&self, sha256: &str) -> Result<bool> {
        Ok(self.store.get(&index(sha256)).await?.is_some())
    }

    /// Keeps `file`, whose content is `sha256`, unless it is kept already.
    ///
    /// # Errors
    /// The store failed, or the file did not hash to `sha256` as it was read.
    pub async fn keep(&self, sha256: &str, file: &File) -> Result<()> {
        if !sha256_name(sha256) {
            return Err(Error::Refused(format!("{sha256:?} names no content")));
        }
        if self.holds(sha256).await? {
            return Ok(());
        }
        let (mut sha, mut parts, mut size) = (Sha256::new(), 0u64, 0u64);
        loop {
            let mut buf = vec![0u8; PART];
            let filled = fill(file, &mut buf, size).map_err(io(sha256))?;
            if filled == 0 {
                break;
            }
            buf.truncate(filled);
            sha.update(&buf);
            self.store.put(&part(sha256, parts), buf).await?;
            parts += 1;
            size += crate::meta::wide(filled);
            if filled < PART {
                break;
            }
        }
        if hex(&sha.finalize()) != sha256 {
            return Err(Error::Refused(format!("{sha256} changed while it was kept")));
        }
        self.store.put(&index(sha256), format!("{parts} {size}").into_bytes()).await?;
        Ok(())
    }

    /// Writes content `sha256` into `out`, checked against its name; `false` where it is not
    /// kept.
    ///
    /// # Errors
    /// The store failed, a part is missing, or what was read does not hash to `sha256`.
    pub async fn fetch(&self, sha256: &str, out: &mut File) -> Result<bool> {
        let Some(idx) = self.store.get(&index(sha256)).await? else { return Ok(false) };
        let text = String::from_utf8_lossy(&idx).into_owned();
        let mut words = text.split(' ').map(str::parse::<u64>);
        let (Some(Ok(parts)), Some(Ok(size)), None) = (words.next(), words.next(), words.next())
        else {
            return Err(Error::Refused(format!("{sha256}: a malformed index")));
        };
        let (mut sha, mut read) = (Sha256::new(), 0u64);
        for n in 0..parts {
            let bytes = self.store.get(&part(sha256, n)).await?;
            let bytes =
                bytes.ok_or_else(|| Error::Refused(format!("{sha256}: part {n} is gone")))?;
            sha.update(&bytes);
            out.write_all(&bytes).map_err(io(sha256))?;
            read += crate::meta::wide(bytes.len());
        }
        if read != size || hex(&sha.finalize()) != sha256 {
            return Err(Error::Refused(format!("{sha256}: what is kept does not match its name")));
        }
        Ok(true)
    }
}

/// Recipe content read from the repository's copy into files of `dir`, each unlinked as it is
/// made, so nothing of it outlives the restore.
pub struct FromRepo<S> {
    kept: Kept<S>,
    handle: tokio::runtime::Handle,
    dir: PathBuf,
}

impl<S> std::fmt::Debug for FromRepo<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FromRepo").field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl<S: ObjectStore> FromRepo<S> {
    /// Content read from `kept`, on `handle`'s runtime, into files of `dir`.
    #[must_use]
    pub fn new(kept: Kept<S>, handle: tokio::runtime::Handle, dir: PathBuf) -> Self {
        Self { kept, handle, dir }
    }
}

impl<S: ObjectStore> ContentSource for FromRepo<S> {
    fn open(&self, sha256: &str) -> std::io::Result<Option<File>> {
        if !sha256_name(sha256) {
            return Ok(None);
        }
        let path = self.dir.join(format!("content-{sha256}"));
        let mut file =
            std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        std::fs::remove_file(&path)?;
        match self.handle.block_on(self.kept.fetch(sha256, &mut file)) {
            Ok(true) => {
                file.rewind()?;
                Ok(Some(file))
            }
            Ok(false) => Ok(None),
            Err(e) => Err(std::io::Error::other(e.to_string())),
        }
    }
}

/// Content from the first of several sources that holds it.
pub struct Chain(pub Vec<Arc<dyn ContentSource>>);

impl std::fmt::Debug for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chain").field("sources", &self.0.len()).finish()
    }
}

impl ContentSource for Chain {
    fn open(&self, sha256: &str) -> std::io::Result<Option<File>> {
        for source in &self.0 {
            if let Some(file) = source.open(sha256)? {
                return Ok(Some(file));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, reason = "a failing test should fail loudly")]

    use super::*;
    use crate::store::MemoryStore;
    use std::io::Read as _;

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    fn file_of(dir: &std::path::Path, bytes: &[u8]) -> File {
        let path = dir.join("f");
        std::fs::write(&path, bytes).unwrap();
        File::open(path).unwrap()
    }

    #[tokio::test]
    async fn content_is_kept_once_in_parts_and_read_back_only_as_its_name_says() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::default());
        let kept = Kept::new(Arc::clone(&store));
        // Two whole parts and a little more.
        let bytes: Vec<u8> = (0..2 * PART + 5).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let name = sha(&bytes);
        assert!(!kept.holds(&name).await.unwrap());
        kept.keep(&name, &file_of(dir.path(), &bytes)).await.unwrap();
        assert!(kept.holds(&name).await.unwrap());
        let names = store.list("").await.unwrap();
        assert_eq!(names.len(), 4, "three parts and the index: {names:?}");
        kept.keep(&name, &file_of(dir.path(), &bytes)).await.unwrap();
        assert_eq!(store.list("").await.unwrap(), names, "kept once");

        let mut out = tempfile::tempfile().unwrap();
        assert!(kept.fetch(&name, &mut out).await.unwrap());
        out.rewind().unwrap();
        let mut back = Vec::new();
        out.read_to_end(&mut back).unwrap();
        assert_eq!(sha(&back), name, "read back whole");

        store.tamper(&format!("{name}/1"), b"not it".to_vec()).unwrap();
        let mut out = tempfile::tempfile().unwrap();
        assert!(kept.fetch(&name, &mut out).await.is_err(), "a part not its own is refused");
        let mut out = tempfile::tempfile().unwrap();
        assert!(!kept.fetch(&"e".repeat(64), &mut out).await.unwrap(), "not kept is not an error");

        let other = sha(b"other");
        assert!(kept.keep(&other, &file_of(dir.path(), b"not other")).await.is_err());
        assert!(!kept.holds(&other).await.unwrap(), "content that is not its name is never whole");
    }

    #[tokio::test]
    async fn a_restore_reads_the_node_s_store_first_then_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::default());
        let name = sha(b"a jar");
        Kept::new(Arc::clone(&store)).keep(&name, &file_of(dir.path(), b"a jar")).await.unwrap();
        let repo =
            FromRepo::new(Kept::new(store), tokio::runtime::Handle::current(), dir.path().into());
        let chain = Chain(vec![Arc::new(crate::NoContent), Arc::new(repo)]);
        let opened = tokio::task::spawn_blocking(move || {
            let mut file = chain.open(&name).unwrap().expect("from the repository");
            let mut back = Vec::new();
            file.read_to_end(&mut back).unwrap();
            (back, chain.open(&"e".repeat(64)).unwrap().is_none())
        })
        .await
        .unwrap();
        assert_eq!(opened, (b"a jar".to_vec(), true));
        let left: Vec<_> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["f"], "nothing of what was read outlives it");
    }
}
