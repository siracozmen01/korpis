// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a workload's kept backups reference (§5.5 of docs/06-storage.md): every object a prune
//! must leave, by its name beneath the workload's prefix. Each kept backup's record is read, and
//! from its root every tree once, however many backups share it, several at a time; a tree names
//! the chunks of its files and the trees of its directories. Recipe content a file refers to is
//! not beneath the prefix, so it is not named here. A record or tree that cannot be read fails the
//! whole: what a prune would delete is only ever judged from a complete answer.

use crate::error::Result;
use crate::repo::Repo;
use crate::seal::Kind;
use crate::store::ObjectStore;
use futures_util::{StreamExt as _, TryStreamExt as _};
use korpis_proto::korpis::backup::v1 as pb;
use std::collections::BTreeSet;

/// How many trees are read at once.
const AT_ONCE: usize = 16;

/// The names of every object the backups `kept` reference, their records included.
///
/// # Errors
/// A record or tree that is missing, does not open, or is not what its name says.
pub async fn referenced<S: ObjectStore>(
    repo: &Repo<S>,
    kept: &[String],
) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    let mut next: Vec<String> = Vec::new();
    for id in kept {
        let record = repo.backup(id).await?;
        names.insert(Kind::Backup.key_of(id));
        if names.insert(Kind::Tree.key_of(&record.root)) {
            next.push(record.root);
        }
    }
    while !next.is_empty() {
        let trees: Vec<pb::Tree> = futures_util::stream::iter(next.drain(..))
            .map(|id| async move { repo.tree(&id).await })
            .buffer_unordered(AT_ONCE)
            .try_collect()
            .await?;
        for e in trees.iter().flat_map(|t| &t.entries) {
            names.extend(e.chunks.iter().map(|c| Kind::Chunk.key_of(c)));
            if !e.subtree.is_empty() && names.insert(Kind::Tree.key_of(&e.subtree)) {
                next.push(e.subtree.clone());
            }
        }
    }
    Ok(names)
}
