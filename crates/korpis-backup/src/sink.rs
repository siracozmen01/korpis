// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a walk's objects go. A backup's walk sends each new object to uploaders, which seal and
//! write several at once while the walk goes on, the walk waiting when they fall behind, so no
//! more than a bounded number of chunks is ever held in memory. A verification's walk keeps its
//! trees and sends nothing.

use crate::error::{Error, Result};
use crate::repo::Repo;
use crate::seal::Kind;
use crate::store::ObjectStore;
use crate::walk::Sink;
use buffa::Message as _;
use korpis_proto::korpis::backup::v1 as pb;
use std::collections::{HashMap, HashSet};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

/// An object on its way to the store.
pub(crate) type Object = (Kind, String, Vec<u8>);

/// A backup's sink: what the repository already has, and the uploaders' queue.
pub(crate) struct Upload<S> {
    pub known: HashSet<String>,
    pub tx: mpsc::Sender<Object>,
    pub repo: Repo<S>,
    pub handle: Handle,
    pub new_chunks: u64,
    pub new_bytes: u64,
}

impl<S: ObjectStore + 'static> Sink for Upload<S> {
    fn has(&mut self, kind: Kind, id: &str) -> bool {
        self.known.contains(&kind.key_of(id))
    }

    fn put(&mut self, kind: Kind, id: &str, plain: Vec<u8>) -> Result<()> {
        self.known.insert(kind.key_of(id));
        if kind == Kind::Chunk {
            self.new_chunks += 1;
            self.new_bytes += crate::meta::wide(plain.len());
        }
        self.tx
            .blocking_send((kind, id.to_owned(), plain))
            .map_err(|_| Error::Refused("the upload stopped".into()))
    }

    fn parent(&mut self, id: &str) -> Result<Option<pb::Tree>> {
        self.handle.block_on(self.repo.tree(id)).map(Some)
    }
}

/// A verification's sink: the trees of what is walked, kept in memory, and no data.
#[derive(Debug, Default)]
pub(crate) struct Dry {
    pub trees: HashMap<String, pb::Tree>,
}

impl Sink for Dry {
    fn has(&mut self, kind: Kind, id: &str) -> bool {
        kind == Kind::Chunk || self.trees.contains_key(id)
    }

    fn put(&mut self, _: Kind, id: &str, plain: Vec<u8>) -> Result<()> {
        let tree =
            pb::Tree::decode_from_slice(&plain).map_err(|e| Error::Corrupt(e.to_string()))?;
        self.trees.insert(id.to_owned(), tree);
        Ok(())
    }

    fn parent(&mut self, _: &str) -> Result<Option<pb::Tree>> {
        Ok(None)
    }
}

/// Seals and writes what arrives on `rx`, `concurrency` at a time, until the walk ends; the
/// bytes written.
pub(crate) async fn upload<S: ObjectStore + 'static>(
    repo: Repo<S>,
    rx: mpsc::Receiver<Object>,
    concurrency: usize,
) -> Result<u64> {
    use futures_util::TryStreamExt as _;
    let objects = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|o| (Ok::<_, Error>(o), rx))
    });
    let written = std::sync::atomic::AtomicU64::new(0);
    objects
        .try_for_each_concurrent(concurrency.max(1), |(kind, id, plain)| {
            let (repo, written) = (repo.clone(), &written);
            async move {
                let n = repo.put(kind, &id, plain).await?;
                written.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
        })
        .await?;
    Ok(written.into_inner())
}
