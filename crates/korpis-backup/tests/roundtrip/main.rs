// SPDX-License-Identifier: AGPL-3.0-or-later

//! A volume backed up, restored, and verified, through a store in memory: every kind of entry
//! comes back as it was, sparse files stay sparse, links stay links, a second backup stores only
//! what changed and reads only what might have, one path restores alone, and a repository that
//! was tampered with or names an entry outside the restore is refused.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod tree;

use buffa::Message as _;
use korpis_backup::keys::KEY_LEN;
use korpis_backup::seal::Kind;
use korpis_backup::store::MemoryStore;
use korpis_backup::{
    Content, Error, Keys, NoContent, Options, Repo, Restore, backup, restore, verify,
};
use korpis_proto::korpis::backup::v1 as pb;
use std::path::Path;
use std::sync::Arc;

fn repo(store: &MemoryStore) -> Repo<MemoryStore> {
    Repo::new(Arc::new(store.clone()), Arc::new(Keys::derive(&[9; KEY_LEN])))
}

fn options(source: &Path, parent: Option<&str>) -> Options {
    Options {
        source: source.to_owned(),
        consistent: true,
        parent: parent.map(str::to_owned),
        content: Content::default(),
        concurrency: 4,
    }
}

fn into(dir: &Path, backup: &str, path: &str) -> Restore {
    Restore {
        backup: backup.into(),
        path: path.into(),
        target: dir.to_owned(),
        content: Arc::new(NoContent),
        concurrency: 4,
    }
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

/// The bytes a file really occupies.
fn allocated(path: &Path) -> u64 {
    std::os::unix::fs::MetadataExt::blocks(&std::fs::metadata(path).unwrap()) * 512
}

#[tokio::test]
async fn a_volume_comes_back_as_it_was_and_a_second_backup_stores_what_changed() {
    let (src, dst) = (scratch(), scratch());
    tree::make(src.path());
    let store = MemoryStore::default();
    let r = repo(&store);
    let first = backup(&r, "b1", options(src.path(), None)).await.unwrap();
    let stats = first.stats.as_option().unwrap();
    assert_eq!(stats.files_unchanged, 0);
    assert!(stats.skipped.is_empty(), "{:?}", stats.skipped);
    assert!(stats.chunks_new >= 9, "a few megabytes each, cut into chunks");
    assert!(stats.bytes_new < 20 << 20, "the holes are not stored");

    let done = restore(&r, into(dst.path(), "b1", "")).await.unwrap();
    assert!(done.files >= 7);
    verify(&r, "b1", "", dst.path().to_owned()).await.unwrap();
    assert!(allocated(&dst.path().join("sparse.img")) < 4 << 20, "sparse stays sparse");
    assert!(allocated(&dst.path().join("hole.img")) < 1 << 20);
    let a = std::fs::metadata(dst.path().join("server.properties")).unwrap();
    let b = std::fs::metadata(dst.path().join("world/properties.link")).unwrap();
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&a),
        std::os::unix::fs::MetadataExt::ino(&b),
        "a link again"
    );
    assert_eq!(
        std::fs::read_link(dst.path().join("dangling")).unwrap(),
        Path::new("/nowhere/at/all")
    );

    std::fs::write(src.path().join("server.properties"), "motd=changed\n").unwrap();
    let mut level = std::fs::read(src.path().join("world/level.dat")).unwrap();
    level.splice(1000..1000, *b"an insertion near the start");
    std::fs::write(src.path().join("world/level.dat"), level).unwrap();
    let second = backup(&r, "b2", options(src.path(), Some("b1"))).await.unwrap();
    let stats = second.stats.as_option().unwrap();
    assert!(stats.files_unchanged >= 3, "unchanged files are not read: {stats:?}");
    assert!(
        stats.bytes_new < 3 << 20,
        "an insertion stores the chunks around it: {}",
        stats.bytes_new
    );
    let again = scratch();
    restore(&r, into(again.path(), "b2", "")).await.unwrap();
    verify(&r, "b2", "", again.path().to_owned()).await.unwrap();
    assert_eq!(std::fs::read(again.path().join("server.properties")).unwrap(), b"motd=changed\n");

    std::fs::write(dst.path().join("world/region/r.0.0.mca"), b"griefed").unwrap();
    let e = verify(&r, "b1", "", dst.path().to_owned()).await.unwrap_err();
    assert!(matches!(e, Error::Mismatch(_)), "{e}");
}

#[tokio::test]
async fn one_path_restores_alone_and_never_over_what_is_there() {
    let (src, dst) = (scratch(), scratch());
    tree::make(src.path());
    let store = MemoryStore::default();
    let r = repo(&store);
    backup(&r, "b1", options(src.path(), None)).await.unwrap();
    restore(&r, into(dst.path(), "b1", "world/region")).await.unwrap();
    assert!(dst.path().join("region/r.0.0.mca").exists());
    verify(&r, "b1", "world/region", dst.path().to_owned()).await.unwrap();
    restore(&r, into(dst.path(), "b1", "world/properties.link")).await.unwrap();
    verify(&r, "b1", "world/properties.link", dst.path().to_owned()).await.unwrap();
    let e = restore(&r, into(dst.path(), "b1", "world/region")).await.unwrap_err();
    assert!(matches!(e, Error::Io { .. }), "never over what is there: {e}");
    let e = restore(&r, into(dst.path(), "b1", "")).await.unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "a whole volume only into an empty directory: {e}");
    let e = restore(&r, into(dst.path(), "b1", "world/../etc")).await.unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "{e}");
}

#[tokio::test]
async fn a_tampered_or_hostile_repository_is_refused() {
    let (src, dst) = (scratch(), scratch());
    tree::make(src.path());
    let store = MemoryStore::default();
    let r = repo(&store);
    backup(&r, "b1", options(src.path(), None)).await.unwrap();
    let chunk = r.ids(Kind::Chunk).await.unwrap().into_iter().next().unwrap();
    let other = r.ids(Kind::Chunk).await.unwrap().into_iter().find(|c| *c != chunk).unwrap();
    let moved = korpis_backup::store::ObjectStore::get(&store, &Kind::Chunk.key_of(&other))
        .await
        .unwrap()
        .unwrap();
    store.tamper(&Kind::Chunk.key_of(&chunk), moved).unwrap();
    let e = restore(&r, into(dst.path(), "b1", "")).await.unwrap_err();
    assert!(matches!(e, Error::Seal { .. }), "{e}");

    let hostile = pb::Tree {
        entries: vec![pb::Entry {
            name: b"..".to_vec(),
            kind: pb::EntryKind::ENTRY_KIND_FILE.into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let encoded = hostile.encode_to_vec();
    let id = r.keys().tree_id(&encoded);
    r.put(Kind::Tree, &id, encoded).await.unwrap();
    r.put_backup(&pb::Backup { id: "evil".into(), root: id, ..Default::default() }).await.unwrap();
    let empty = scratch();
    let e = restore(&r, into(empty.path(), "evil", "")).await.unwrap_err();
    assert!(matches!(e, Error::Corrupt(_)), "{e}");
    assert_eq!(std::fs::read_dir(empty.path()).unwrap().count(), 0, "nothing was created");
}

/// Recipe content kept by the content store, by sha256.
struct Recipe(std::path::PathBuf);

impl korpis_backup::ContentSource for Recipe {
    fn open(&self, sha256: &str) -> std::io::Result<Option<std::fs::File>> {
        Ok(std::fs::File::open(self.0.join(sha256)).ok())
    }
}

#[tokio::test]
async fn recipe_content_is_recorded_by_reference_and_restored_from_where_it_is_kept() {
    use sha2::Digest as _;
    let (src, dst, kept) = (scratch(), scratch(), scratch());
    let jar = tree::noise(2 << 20, 7);
    std::fs::write(src.path().join("server.jar"), &jar).unwrap();
    let sha: [u8; 32] = sha2::Sha256::digest(&jar).into();
    let hex = sha.iter().fold(String::new(), |s, b| s + &format!("{b:02x}"));
    std::fs::write(kept.path().join(&hex), &jar).unwrap();
    let store = MemoryStore::default();
    let r = repo(&store);
    let mut o = options(src.path(), None);
    o.content = Content::new([(2 << 20, sha)]);
    let b = backup(&r, "b1", o).await.unwrap();
    assert_eq!(b.stats.as_option().unwrap().bytes_new, 0, "stored once per repository, not here");
    let e = restore(&r, into(dst.path(), "b1", "")).await.unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "{e}");
    let fresh = scratch();
    let mut with = into(fresh.path(), "b1", "");
    with.content = Arc::new(Recipe(kept.path().to_owned()));
    restore(&r, with).await.unwrap();
    verify(&r, "b1", "", fresh.path().to_owned()).await.unwrap();
}
