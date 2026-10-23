// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent's leases against a real control plane (§4.5 of docs/02-architecture.md): asked for
//! as soon as a workload is held, renewed under the same epoch, and released through the
//! journal once the workload has stopped acting under it.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::join::{Join, join};
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::lease::{Keeper, Leases, Standing};
use korpis_agent::session::{Event, Session};
use korpis_agent::state::StateDir;
use std::sync::Arc;
use std::time::Duration;

/// The standing of `workload`'s lease once `pick` takes it, within twenty seconds: a machine
/// busy with other work slows the renewals the standing waits on.
async fn standing(leases: &Leases, workload: &str, pick: impl Fn(&Standing) -> bool) -> Standing {
    for _ in 0..400 {
        let s = leases.standing(workload);
        if pick(&s) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the lease of {workload} never reached the standing awaited");
}

#[tokio::test]
async fn a_held_workload_is_leased_renewed_under_one_epoch_and_released_through_the_journal() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let (token, ca) = cp.token("fra-6").await;
    let dir = tempfile::tempdir().unwrap();
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    let identity = join(&state, &request).await.unwrap();
    let journal = JournalHandle::open(&state.root().join("journal"), Bounds::default()).unwrap();
    let (session, link) = Session::new(identity, state, Arc::new(facts), journal.clone());
    let mut events = link.events.subscribe();
    let leases = Leases::new(journal.clone());
    let keeper = Keeper::new(leases.clone(), link.held.clone(), link.inbound, link.outbound);
    let _session = tokio::spawn(session.run());
    let _keeper = tokio::spawn(keeper.run());
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;

    let workload = cp.declare(&project, "mc").await;
    assert_eq!(leases.standing(&workload), Standing::Pending, "nothing is held yet");
    let held = standing(&leases, &workload, |s| matches!(s, Standing::Held { .. })).await;
    let Standing::Held { epoch } = held else { unreachable!() };
    let issued: i64 = cp.scalar("SELECT count(*) FROM effect WHERE action = 'lease.issue'").await;
    assert_eq!(issued, 1);

    // The harness's term is two seconds: three seconds later the lease is still held, renewed
    // under the same epoch, and no second epoch was issued.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(leases.standing(&workload), Standing::Held { epoch });
    let issued: i64 = cp.scalar("SELECT count(*) FROM effect WHERE action = 'lease.issue'").await;
    assert_eq!(issued, 1, "renewals are not issues");

    assert_eq!(leases.release(&workload).await.unwrap(), Some(epoch));
    for _ in 0..100 {
        let released: i64 =
            cp.scalar("SELECT count(*) FROM lease WHERE released_at IS NOT NULL").await;
        if released == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the release never reached the control plane");
}

#[tokio::test]
async fn an_agent_asked_while_a_restore_settles_presents_its_signed_epochs_and_raises_the_floor() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let (token, ca) = cp.token("fra-7").await;
    let dir = tempfile::tempdir().unwrap();
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    let identity = join(&state, &request).await.unwrap();
    let journal = JournalHandle::open(&state.root().join("journal"), Bounds::default()).unwrap();
    let (session, link) =
        Session::new(identity.clone(), state.clone(), Arc::new(facts), journal.clone());
    let leases = Leases::new(journal.clone());
    let keeper = Keeper::new(leases.clone(), link.held.clone(), link.inbound, link.outbound);
    let first = tokio::spawn(session.run());
    let _keeper = tokio::spawn(keeper.run());
    let workload = cp.declare(&project, "mc").await;
    let Standing::Held { epoch } =
        standing(&leases, &workload, |s| matches!(s, Standing::Held { .. })).await
    else {
        unreachable!()
    };
    let mut held = link.held.clone();
    let signed = |h: &korpis_agent::held::HeldSet| {
        h.assignments.get(&workload).is_some_and(|a| a.lease_epoch == epoch)
    };
    tokio::time::timeout(Duration::from_secs(10), held.wait_for(signed)).await.unwrap().unwrap();
    first.abort();

    // A restore that knew no epoch settles; the agent, reconnecting, is asked to present.
    cp.execute("UPDATE epoch_state SET floor = 0, settling_until = now() + interval '1 minute'")
        .await;
    let (session, link) = Session::new(identity, state, Arc::new(facts), journal);
    let mut events = link.events.subscribe();
    let _second = tokio::spawn(session.run());
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    for _ in 0..100 {
        let floor: i64 = cp.scalar("SELECT floor FROM epoch_state").await;
        if floor == i64::try_from(epoch).unwrap() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the presented epoch {epoch} never raised the floor");
}
