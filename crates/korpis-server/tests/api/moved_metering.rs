// SPDX-License-Identifier: AGPL-3.0-or-later

//! The meter of a moved workload is cut at its move: a sample the source took under the epoch it
//! released, ending after the move, is not believed, one ending before it is, and the
//! destination's first sample counts from nothing, so no interval is counted twice or not at all.

use crate::journal::{ack, send, until};
use crate::metering::sample_at;
use crate::migrating::{cut_over, moving, through_verify};
use std::time::{Duration, SystemTime};

#[tokio::test]
async fn a_moved_workload_s_meter_is_cut_at_its_move() {
    let mut m = Box::pin(moving()).await;
    through_verify(&mut m, ("sha256:same", "sha256:same"), 4096).await;
    cut_over(&mut m).await;
    let before = m.h.effects("journal.rejected").await.len();
    let hour = Duration::from_secs(3600);

    let (s, seq) = &mut m.source;
    let ended_before = sample_at(*seq, &m.db, SystemTime::now() - hour - hour / 12, 10);
    let runs_past = sample_at(*seq + 1, &m.db, SystemTime::now() - hour / 2, 20);
    send(s, vec![ended_before, runs_past]).await;
    assert_eq!(until(s, ack).await, *seq + 1);
    *seq += 2;
    let after = m.h.effects("journal.rejected").await.len();
    assert_eq!(after, before + 1, "the sample running past the move, and not the one before it");

    let (d, dseq) = &mut m.destination;
    send(d, vec![sample_at(*dseq, &m.db, SystemTime::now() - hour / 2, 5)]).await;
    assert_eq!(until(d, ack).await, *dseq);
    let last = m.h.effects("journal.rejected").await.len();
    assert_eq!(last, before + 1, "the destination's first is believed");
}
