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

fn migration(phase: Phase) -> Migration {
    let at = std::time::UNIX_EPOCH;
    Migration {
        id: Uuid::nil(),
        plan: Uuid::nil(),
        workload: Uuid::nil(),
        source: Uuid::from_u128(1),
        destination: Uuid::from_u128(2),
        phase,
        phase_since: at,
        hold: DEFAULT_HOLD,
        hold_until: None,
        outcome: None,
        made_at: at,
        reported: Reported::default(),
        moved_at: None,
    }
}

#[test]
fn a_phase_advances_once_every_side_acting_in_it_reported_it_done() {
    let world = |manifests: (Option<&str>, Option<&str>)| {
        let mut v =
            MigrationVolume::new(Uuid::nil(), "world".into(), Crossing::Tree, (1, "w".into()));
        v.manifests = (manifests.0.map(str::to_owned), manifests.1.map(str::to_owned));
        v
    };
    let both = |p| Reported { source: Some(p), destination: Some(p) };
    let prepare = migration(Phase::Prepare);
    assert_eq!(next(&prepare, Reported::default(), &[]), Next::Wait);
    let ready = Reported { destination: Some(Phase::Prepare), ..Reported::default() };
    assert_eq!(next(&prepare, ready, &[]), Next::Advance(Phase::Replicate), "the source idles");
    let replicate = migration(Phase::Replicate);
    let half = Reported { source: Some(Phase::Replicate), destination: Some(Phase::Prepare) };
    assert_eq!(next(&replicate, half, &[]), Next::Wait, "both copy");
    let verify = migration(Phase::Verify);
    let agreed = [world((Some("sha256:a"), Some("sha256:a")))];
    assert_eq!(next(&verify, both(Phase::Verify), &agreed), Next::Advance(Phase::Cutover));
    let partial = [world((Some("sha256:a"), None))];
    assert_eq!(next(&verify, both(Phase::Verify), &partial), Next::Wait);
    let differ = [world((Some("sha256:a"), Some("sha256:b")))];
    let Next::Abandon(why) = next(&verify, both(Phase::Verify), &differ) else { panic!() };
    assert!(why.contains("world"), "{why}");
    let release = migration(Phase::Release);
    let deleted = Reported { source: Some(Phase::Release), ..Reported::default() };
    assert_eq!(next(&release, deleted, &[]), Next::Complete);
}

#[test]
fn a_failed_phase_abandons_before_cutover_and_rolls_back_at_it() {
    assert_eq!(Side::of(&migration(Phase::Prepare), Uuid::from_u128(2)), Some(Side::Destination));
    assert_eq!(Side::of(&migration(Phase::Prepare), Uuid::from_u128(3)), None);
    let why = "the disk filled";
    let m = migration(Phase::FinalSync);
    assert_eq!(failed(&m, Phase::FinalSync, why), Next::Abandon(why.into()));
    assert_eq!(failed(&m, Phase::Replicate, why), Next::Wait, "late");
    let cut = migration(Phase::Cutover);
    assert_eq!(failed(&cut, Phase::Cutover, why), Next::RollBack(why.into()));
    assert_eq!(failed(&migration(Phase::Release), Phase::Release, why), Next::Wait, "again");
    assert!(acts(Phase::Prepare, Side::Destination) && !acts(Phase::Prepare, Side::Source));
}
