// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::extension::block::BlockProtocol;
use crate::node::driver::{Fencing, StorageBackend, StorageCaps};

fn class(backend: StorageBackend, features: &[StorageFeature]) -> StorageClass {
    StorageClass {
        name: "world".into(),
        backend,
        capabilities: StorageCaps {
            features: features.iter().copied().collect(),
            fencing: Fencing::None,
        },
        capacity_bytes: 1 << 40,
        free_bytes: 1 << 39,
        store: None,
        block: None,
    }
}

#[test]
fn the_phases_move_in_order_and_the_placement_moves_at_cutover() {
    assert_eq!(Phase::Prepare.next(), Some(Phase::Replicate));
    assert_eq!(Phase::Verify.next(), Some(Phase::Cutover));
    assert_eq!(Phase::Release.next(), None);
    for p in Phase::ALL {
        assert_eq!(Phase::parse(p.as_str()), Some(p));
    }
    assert!(!Phase::Verify.moved() && Phase::Cutover.moved() && Phase::Release.moved());
    assert_eq!(Outcome::parse("rolled_back"), Some(Outcome::RolledBack));
    assert_eq!(Crossing::parse("tree"), Some(Crossing::Tree));
}

#[test]
fn a_volume_crosses_by_increments_by_its_tree_or_not_at_all() {
    use StorageFeature::{IncrementalSend, TreeSync};
    let btrfs = class(StorageBackend::Btrfs, &[IncrementalSend, TreeSync]);
    let dir = class(StorageBackend::Directory, &[TreeSync]);
    let bare = class(StorageBackend::Directory, &[]);
    assert_eq!(crossing(&btrfs, Some(&btrfs)), Ok(Crossing::Increments));
    assert_eq!(crossing(&btrfs, Some(&dir)), Ok(Crossing::Tree), "one side sends no increments");
    assert_eq!(crossing(&dir, Some(&dir)), Ok(Crossing::Tree));
    let refused = crossing(&dir, Some(&bare)).unwrap_err();
    assert_eq!(refused, Uncrossable::NoTreeSync(vec!["destination"]));
    assert!(refused.to_string().contains("the destination cannot sync"), "{refused}");
    assert_eq!(crossing(&dir, None), Err(Uncrossable::NotOffered));

    let shared = StorageClass { store: Some("nfs:/srv".into()), ..bare.clone() };
    assert_eq!(crossing(&shared, Some(&shared)), Ok(Crossing::Nothing), "the same storage");
    let block = StorageClass { block: Some(BlockProtocol::Iscsi), ..bare };
    assert_eq!(crossing(&block, Some(&block)), Ok(Crossing::Nothing));
}

#[test]
fn a_pass_is_small_under_a_hundredth_or_sixty_four_mebibytes_or_the_tenth() {
    let gib = 1u64 << 30;
    assert!(small(SMALL_PASS, gib, 1), "a small volume's pass under the floor");
    assert!(!small(SMALL_PASS + 1, gib, 1));
    assert!(small(400 << 20, 40 * gib, 2), "under a hundredth of 40 GiB");
    assert!(!small(500 << 20, 40 * gib, 2));
    assert!(small(u64::MAX, gib, MOST_PASSES), "the tenth ends it whatever it sent");
}
