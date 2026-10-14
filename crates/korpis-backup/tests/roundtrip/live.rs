// SPDX-License-Identifier: AGPL-3.0-or-later

//! What kept backups reference (§5.5 of docs/06-storage.md): everything a restore of them needs,
//! so deleting the rest leaves each one whole; and what only a forgotten backup held is not named.

use super::{into, options, repo, scratch, tree};
use korpis_backup::seal::Kind;
use korpis_backup::store::{MemoryStore, ObjectStore as _};
use korpis_backup::{Error, backup, live, restore, verify};
use std::collections::BTreeSet;

#[tokio::test]
async fn deleting_what_the_kept_backups_do_not_reference_leaves_them_whole() {
    let (src, dst) = (scratch(), scratch());
    tree::make(src.path());
    let store = MemoryStore::default();
    let r = repo(&store);
    backup(&r, "b1", options(src.path(), None)).await.unwrap();
    std::fs::remove_dir_all(src.path().join("world/region")).unwrap();
    std::fs::write(src.path().join("server.properties"), "motd=changed\n").unwrap();
    backup(&r, "b2", options(src.path(), Some("b1"))).await.unwrap();

    let kept = live::referenced(&r, &["b2".into()]).await.unwrap();
    let all: BTreeSet<String> = store.list("").await.unwrap().into_iter().collect();
    let garbage: Vec<&String> = all.difference(&kept).collect();
    assert!(kept.is_subset(&all), "everything named exists");
    assert!(garbage.contains(&&Kind::Backup.key_of("b1")), "the forgotten record goes");
    assert!(garbage.iter().any(|n| n.starts_with("chunks/")), "the region file's chunks go");
    assert!(garbage.iter().any(|n| n.starts_with("trees/")), "b1's root and world go");

    for name in garbage {
        store.remove(name).unwrap();
    }
    restore(&r, into(dst.path(), "b2", "")).await.unwrap();
    verify(&r, "b2", "", dst.path().to_owned()).await.unwrap();
    let e = live::referenced(&r, &["b1".into()]).await.unwrap_err();
    assert!(matches!(e, Error::Corrupt(_)), "a missing record fails the whole: {e}");
}
