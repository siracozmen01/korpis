// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file's data. Only the runs the filesystem says hold data are read (`SEEK_DATA` and
//! `SEEK_HOLE`), so a sparse file is stored as its data and restored sparse; each run is cut at
//! content-defined boundaries, so an insertion moves the chunks around it and no others. The
//! whole content, holes read as zeros, is hashed as it goes, for verification. A file whose bytes
//! are recipe content is recorded by that content's digest and stores nothing (§5.2 of
//! docs/06-storage.md).

use crate::error::{Error, Result};
use crate::keys::Keys;
use crate::seal::Kind;
use crate::walk::Sink;
use korpis_proto::korpis::backup::v1 as pb;
use rustix::fs::SeekFrom;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::os::unix::fs::FileExt as _;

/// The smallest, typical, and largest chunk. A megabyte on average: large enough that a 40 GB
/// world is forty thousand objects, small enough that a day's changes are a small part of it.
const MIN: usize = 512 << 10;
const AVG: usize = 1 << 20;
const MAX: usize = 8 << 20;

static ZEROS: [u8; 64 << 10] = [0; 64 << 10];

/// Recipe content a file may be byte-identical to: sha256 digests by size, so only a file of a
/// size some content has is hashed for it.
#[derive(Debug, Clone, Default)]
pub struct Content {
    by_size: BTreeMap<u64, BTreeSet<[u8; 32]>>,
}

impl Content {
    /// The content of these sizes and sha256 digests.
    #[must_use]
    pub fn new(files: impl IntoIterator<Item = (u64, [u8; 32])>) -> Self {
        let mut by_size: BTreeMap<u64, BTreeSet<[u8; 32]>> = BTreeMap::new();
        for (size, digest) in files {
            by_size.entry(size).or_default().insert(digest);
        }
        Self { by_size }
    }
}

/// What reading a file found.
#[derive(Debug, Default)]
pub(crate) struct Data {
    pub chunks: Vec<String>,
    pub extents: Vec<pb::Extent>,
    pub digest: Vec<u8>,
    pub content: String,
    /// It had less data than it said when the read began: it changed while it was read.
    pub short: bool,
}

/// `bytes` in lowercase hexadecimal.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The runs of `file` that hold data, below `size`.
fn extents(file: &File, size: u64) -> rustix::io::Result<Vec<(u64, u64)>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < size {
        let data = match rustix::fs::seek(file, SeekFrom::Data(at)) {
            Ok(d) => d,
            Err(rustix::io::Errno::NXIO) => break,
            // A filesystem that cannot say has data everywhere.
            Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOTSUP) => {
                return Ok(vec![(0, size)]);
            }
            Err(e) => return Err(e),
        };
        if data >= size {
            break;
        }
        let hole = rustix::fs::seek(file, SeekFrom::Hole(data))?.min(size);
        out.push((data, hole - data));
        at = hole;
    }
    Ok(out)
}

/// `left` bytes of a file from `at`, hashed into `digest` as they are read.
struct Run<'a> {
    file: &'a File,
    at: u64,
    left: u64,
    digest: &'a mut blake3::Hasher,
    sha: Option<&'a mut Sha256>,
}

impl std::io::Read for Run<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let want = usize::try_from(self.left).unwrap_or(usize::MAX).min(buf.len());
        let Some(buf) = buf.get_mut(..want) else { return Ok(0) };
        let n = self.file.read_at(buf, self.at)?;
        let read = buf.get(..n).unwrap_or_default();
        self.digest.update(read);
        if let Some(sha) = self.sha.as_deref_mut() {
            sha.update(read);
        }
        self.at += crate::meta::wide(n);
        self.left -= crate::meta::wide(n);
        Ok(n)
    }
}

/// Feeds `n` zero bytes, a hole, to the hashes.
fn zeros(digest: &mut blake3::Hasher, mut sha: Option<&mut Sha256>, mut n: u64) {
    while n > 0 {
        let take = usize::try_from(n).unwrap_or(ZEROS.len()).min(ZEROS.len());
        let z = ZEROS.get(..take).unwrap_or_default();
        digest.update(z);
        if let Some(s) = sha.as_deref_mut() {
            s.update(z);
        }
        n -= crate::meta::wide(take);
    }
}

/// The sha256 and digest of the whole content, holes as zeros.
fn hashed(
    keys: &Keys,
    file: &File,
    size: u64,
    runs: &[(u64, u64)],
) -> std::io::Result<([u8; 32], Vec<u8>)> {
    let (mut digest, mut sha) = (keys.digester(), Sha256::new());
    let mut at = 0;
    for &(offset, len) in runs {
        zeros(&mut digest, Some(&mut sha), offset.saturating_sub(at));
        let mut run = Run { file, at: offset, left: len, digest: &mut digest, sha: Some(&mut sha) };
        std::io::copy(&mut run, &mut std::io::sink())?;
        at = offset + len;
    }
    zeros(&mut digest, Some(&mut sha), size.saturating_sub(at));
    Ok((sha.finalize().into(), digest.finalize().as_bytes().to_vec()))
}

/// Reads `file`, of `size` bytes, sending the chunks the repository lacks to `sink`.
pub(crate) fn read<K: Sink>(
    keys: &Keys,
    sink: &mut K,
    file: &File,
    size: u64,
    content: &Content,
) -> Result<Data> {
    let io = |e: std::io::Error| Error::Io { path: String::new(), source: e };
    let runs = extents(file, size).map_err(|e| io(e.into()))?;
    if let Some(known) = content.by_size.get(&size) {
        let (sha, digest) = hashed(keys, file, size, &runs).map_err(io)?;
        if known.contains(&sha) {
            let content = hex(&sha);
            return Ok(Data { digest, content, ..Data::default() });
        }
    }
    let mut data = Data::default();
    let mut digest = keys.digester();
    let mut at = 0;
    for (offset, len) in runs {
        zeros(&mut digest, None, offset.saturating_sub(at));
        let run = Run { file, at: offset, left: len, digest: &mut digest, sha: None };
        let mut read = 0;
        for chunk in fastcdc::v2020::StreamCDC::new(run, MIN, AVG, MAX) {
            let chunk = chunk.map_err(|e| io(std::io::Error::other(e.to_string())))?;
            read += crate::meta::wide(chunk.length);
            let id = keys.chunk_id(&chunk.data);
            if !sink.has(Kind::Chunk, &id) {
                sink.put(Kind::Chunk, &id, chunk.data)?;
            }
            data.chunks.push(id);
        }
        data.short |= read < len;
        data.extents.push(pb::Extent { offset, length: read, ..Default::default() });
        at = offset + read;
    }
    zeros(&mut digest, None, size.saturating_sub(at));
    data.digest = digest.finalize().as_bytes().to_vec();
    // A file that is data throughout records no runs.
    if let [only] = data.extents.as_slice()
        && only.offset == 0
        && only.length == size
    {
        data.extents.clear();
    }
    Ok(data)
}
