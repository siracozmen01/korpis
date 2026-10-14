// SPDX-License-Identifier: AGPL-3.0-or-later

//! A backup read without restoring it: a directory's entries, and a file's bytes, sparse ones
//! included, exactly as the volume held them; what is not a directory or a file is refused.

use super::{options, repo, scratch, tree};
use korpis_backup::store::MemoryStore;
use korpis_backup::{Error, backup, browse};
use korpis_proto::korpis::backup::v1 as pb;
use std::path::Path;

/// The bytes of `path` in backup `b1`, read through the repository.
async fn read(r: &korpis_backup::Repo<MemoryStore>, path: &str) -> Vec<u8> {
    let e = browse::file(r, "b1", path).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let size = e.size;
    // The sender goes with the reading, so the collecting ends when it does.
    let reading = async move { browse::send(r, &e, &tx).await };
    let collecting = async {
        let mut all = Vec::new();
        while let Some(piece) = rx.recv().await {
            all.extend(piece);
        }
        all
    };
    let (sent, all) = tokio::join!(reading, collecting);
    assert_eq!(sent.unwrap(), size);
    all
}

fn names(entries: &[pb::Entry]) -> Vec<String> {
    let mut n: Vec<String> =
        entries.iter().map(|e| String::from_utf8_lossy(&e.name).into()).collect();
    n.sort();
    n
}

#[tokio::test]
async fn a_backup_is_browsed_and_its_files_read_as_the_volume_held_them() {
    let src = scratch();
    tree::make(src.path());
    let store = MemoryStore::default();
    let r = repo(&store);
    backup(&r, "b1", options(src.path(), None)).await.unwrap();

    let root = browse::list(&r, "b1", "").await.unwrap();
    let on_disk: Vec<String> = {
        let mut n: Vec<String> = std::fs::read_dir(src.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        n.sort();
        n
    };
    assert_eq!(names(&root), on_disk);
    assert_eq!(
        names(&browse::list(&r, "b1", "/world/").await.unwrap()),
        names(&browse::list(&r, "b1", "world").await.unwrap())
    );
    for path in ["server.properties", "world/level.dat", "sparse.img", "hole.img"] {
        assert_eq!(
            read(&r, path).await,
            std::fs::read(Path::new(src.path()).join(path)).unwrap(),
            "{path}"
        );
    }
    let e = browse::list(&r, "b1", "server.properties").await.unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "a file is not browsed: {e}");
    let e = browse::file(&r, "b1", "world").await.unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "a directory is not read: {e}");
    let e = browse::file(&r, "b1", "world/../server.properties").await.unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "{e}");
}
