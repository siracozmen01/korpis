// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node removed while it takes part in a migration ends the migration, since it will report no
//! other phase: a destination removed before the move abandons it, the workload placed where it
//! ran; one removed after the move, while the source still holds its copies, rolls it back there.

use crate::dependencies::placement;
use crate::migrating::{cut_over, moving, shown, through_verify};
use crate::support::{Harness, mutation};
use korpis_proto::korpis::v1 as pb;

/// Removes node `node` as compromised.
async fn removed(h: &Harness, node: &str) {
    let req = pb::RemoveNodeRequest {
        mutation: mutation(&format!("remove-{node}")).into(),
        node: node.to_owned(),
        compromised: true,
        ..Default::default()
    };
    h.nodes(h.root()).remove_node(req).await.unwrap();
}

#[tokio::test]
async fn a_destination_removed_before_the_move_abandons_its_migration() {
    let m = Box::pin(moving()).await;
    let source = placement(&m.h, &m.db).await.node;
    removed(&m.h, &m.told.node).await;
    let ended = shown(&m.h, &m.db).await;
    assert_eq!(ended.outcome.as_known(), Some(pb::MigrationOutcome::MIGRATION_OUTCOME_ABANDONED));
    assert!(ended.reason.contains("destination node was removed"), "{}", ended.reason);
    assert_eq!(placement(&m.h, &m.db).await.node, source, "where it ran");
}

#[tokio::test]
async fn a_destination_removed_after_the_move_rolls_its_migration_back() {
    let mut m = Box::pin(moving()).await;
    let source = placement(&m.h, &m.db).await.node;
    through_verify(&mut m, ("sha256:same", "sha256:same"), 4096).await;
    cut_over(&mut m).await;
    removed(&m.h, &m.told.node).await;
    let ended = shown(&m.h, &m.db).await;
    let back = Some(pb::MigrationOutcome::MIGRATION_OUTCOME_ROLLED_BACK);
    assert_eq!(ended.outcome.as_known(), back, "{}", ended.reason);
    assert_eq!(
        placement(&m.h, &m.db).await.node,
        source,
        "back on the source, which held its copies"
    );
}
