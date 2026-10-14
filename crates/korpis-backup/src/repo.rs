// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's repository: its objects in a store, read and written sealed under its keys.
//! Everything read is checked before it is used: an object opens only under its own name, a
//! chunk or tree is only what its identifier says, and a tree names only entries a restore may
//! create.

use crate::error::{Error, Result};
use crate::keys::Keys;
use crate::name::check_tree;
use crate::seal::{Kind, open, seal};
use crate::store::ObjectStore;
use buffa::Message as _;
use korpis_proto::korpis::backup::v1 as pb;
use std::collections::HashSet;
use std::sync::Arc;

/// A repository: a store beneath one workload's prefix, and the keys of that workload.
#[derive(Debug)]
pub struct Repo<S> {
    store: Arc<S>,
    keys: Arc<Keys>,
}

impl<S> Clone for Repo<S> {
    fn clone(&self) -> Self {
        Self { store: Arc::clone(&self.store), keys: Arc::clone(&self.keys) }
    }
}

impl<S: ObjectStore> Repo<S> {
    /// The repository in `store` under `keys`.
    pub fn new(store: Arc<S>, keys: Arc<Keys>) -> Self {
        Self { store, keys }
    }

    /// The keys.
    #[must_use]
    pub fn keys(&self) -> &Arc<Keys> {
        &self.keys
    }

    /// The store.
    #[must_use]
    pub fn store(&self) -> &Arc<S> {
        &self.store
    }

    /// The identifiers of every object of `kind` there is.
    pub async fn ids(&self, kind: Kind) -> Result<HashSet<String>> {
        let names = self.store.list(kind.prefix()).await?;
        Ok(names
            .into_iter()
            .filter_map(|n| n.strip_prefix(kind.prefix()).map(str::to_owned))
            .collect())
    }

    /// Seals and writes `plain` as `kind`/`id`; how many bytes were written. Sealing compresses
    /// and encrypts, so it runs off the threads that wait on the store.
    pub async fn put(&self, kind: Kind, id: &str, plain: Vec<u8>) -> Result<u64> {
        let name = kind.key_of(id);
        let (keys, owned) = (Arc::clone(&self.keys), id.to_owned());
        let sealed = tokio::task::spawn_blocking(move || seal(&keys, kind, &owned, &plain))
            .await
            .map_err(|e| Error::Refused(format!("sealing stopped: {e}")))?
            .map_err(|source| Error::Seal { name: name.clone(), source })?;
        let len = u64::try_from(sealed.len()).unwrap_or(u64::MAX);
        self.store.put(&name, sealed).await?;
        Ok(len)
    }

    /// The opened bytes of `kind`/`id`.
    async fn get(&self, kind: Kind, id: &str) -> Result<Vec<u8>> {
        let name = kind.key_of(id);
        let sealed = self
            .store
            .get(&name)
            .await?
            .ok_or_else(|| Error::Corrupt(format!("{name} is missing")))?;
        open(&self.keys, kind, id, &sealed).map_err(|source| Error::Seal { name, source })
    }

    /// The bytes of chunk `id`, checked against it.
    pub async fn chunk(&self, id: &str) -> Result<Vec<u8>> {
        let data = self.get(Kind::Chunk, id).await?;
        if self.keys.chunk_id(&data) != id {
            return Err(Error::Corrupt(format!("chunk {id} holds other bytes")));
        }
        Ok(data)
    }

    /// Tree `id`, checked against it and for what it names.
    pub async fn tree(&self, id: &str) -> Result<pb::Tree> {
        let encoded = self.get(Kind::Tree, id).await?;
        if self.keys.tree_id(&encoded) != id {
            return Err(Error::Corrupt(format!("tree {id} holds another tree")));
        }
        let tree = pb::Tree::decode_from_slice(&encoded)
            .map_err(|e| Error::Corrupt(format!("tree {id}: {e}")))?;
        check_tree(&tree)?;
        Ok(tree)
    }

    /// Backup `id`'s record.
    pub async fn backup(&self, id: &str) -> Result<pb::Backup> {
        let encoded = self.get(Kind::Backup, id).await?;
        let backup = pb::Backup::decode_from_slice(&encoded)
            .map_err(|e| Error::Corrupt(format!("backup {id}: {e}")))?;
        if backup.id != id {
            return Err(Error::Corrupt(format!("backup {id} records another backup")));
        }
        Ok(backup)
    }

    /// Writes backup `b`'s record, which makes it whole.
    pub async fn put_backup(&self, b: &pb::Backup) -> Result<u64> {
        self.put(Kind::Backup, &b.id, b.encode_to_vec()).await
    }
}
