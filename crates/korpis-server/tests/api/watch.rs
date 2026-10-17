// SPDX-License-Identifier: AGPL-3.0-or-later

//! Watching a workload (§4.3 of docs/02-architecture.md): its status now, then each change as
//! the database announces it, read with the watcher's authority as it is at that moment, and
//! paid for once by a use-limited grant.

use crate::lifecycle::workload;
use crate::links::in_an_hour;
use crate::support::{Harness, mutation, reason};
use connectrpc::ConnectError;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::scope::Kind;
use std::time::Duration;

type Stream = connectrpc::client::ServerStream<
    <connectrpc::client::HttpClient as connectrpc::client::ClientTransport>::ResponseBody,
    pb::__buffa::view::WatchWorkloadResponseView<'static>,
>;

async fn open(h: &Harness, config: connectrpc::client::ClientConfig, w: &str) -> Stream {
    let workloads = h.workloads(config);
    let req = pb::WatchWorkloadRequest { workload: w.to_owned(), ..Default::default() };
    workloads.watch_workload(req).await.unwrap()
}

/// The next status, or the error that ended the stream; panics after five seconds of silence.
async fn next(stream: &mut Stream) -> Result<pb::WorkloadStatus, ConnectError> {
    let got = tokio::time::timeout(Duration::from_secs(5), stream.message()).await;
    let message = got.expect("a status within five seconds")?.expect("the stream is open");
    Ok(message.to_owned_message().status.into_option().unwrap())
}

#[tokio::test]
async fn a_watch_starts_with_the_status_now_and_hears_each_change() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let mut stream = open(&h, h.root(), &w).await;
    let now = next(&mut stream).await.unwrap();
    assert_eq!(now.convergence.as_known(), Some(pb::Convergence::CONVERGENCE_UNPLACED));
    // Retry passes and a resync run meanwhile; none of them changes what is read, so none of
    // them is sent.
    let quiet = tokio::time::timeout(Duration::from_millis(1500), stream.message()).await;
    assert!(quiet.is_err(), "nothing changed, so nothing was sent");
    let node = h.node(None, "fra-3", &[]).await;
    let placed = next(&mut stream).await.unwrap();
    assert_eq!(placed.placement.as_option().unwrap().node, node, "the bind was announced");
}

#[tokio::test]
async fn a_watch_ends_with_the_refusal_a_fresh_read_would_get() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let (token, secret) = h.bare_token().await;
    let grant = h.issue_to(token, "g", &["workload.read"]).await;
    let mut stream = open(&h, h.config(Some(secret.expose())), &w).await;
    next(&mut stream).await.unwrap();
    h.grants(h.root())
        .revoke_grant(pb::RevokeGrantRequest {
            mutation: mutation("r").into(),
            grant,
            ..Default::default()
        })
        .await
        .unwrap();
    h.node(None, "fra-3", &[]).await;
    let e = next(&mut stream).await.unwrap_err();
    assert_eq!(reason(&e), "object.not_found", "revoked mid-watch, it ends as a read would");
}

#[tokio::test]
async fn nobody_watches_what_they_may_not_read() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let (_, secret) = h.bare_token().await;
    let workloads = h.workloads(h.config(Some(secret.expose())));
    let req = pb::WatchWorkloadRequest { workload: w, ..Default::default() };
    let refused = match workloads.watch_workload(req).await {
        Err(e) => e,
        Ok(mut stream) => next(&mut stream).await.unwrap_err(),
    };
    assert_eq!(reason(&refused), "object.not_found");
}

#[tokio::test]
async fn a_watch_through_a_link_spends_one_use_however_many_changes_it_hears() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let request = pb::IssueGrantRequest {
        mutation: mutation("share").into(),
        actions: vec!["workload.read".to_owned()],
        scope: pb::Scope { kind: Some(Kind::Workload(w.clone())), ..Default::default() }.into(),
        conditions: pb::Conditions {
            expires_at: Some(in_an_hour()).into(),
            max_uses: Some(1),
            ..Default::default()
        }
        .into(),
        link_form: true,
        ..Default::default()
    };
    let issued = h.grants(h.root()).issue_grant(request).await.unwrap().into_owned();
    let link = issued.link_token;
    let mut stream = open(&h, h.config(Some(&link)), &w).await;
    let now = next(&mut stream).await.unwrap();
    let fresh = pb::GetWorkloadRequest { id: w.clone(), ..Default::default() };
    let spent = h.workloads(h.config(Some(&link))).get_workload(fresh).await.unwrap_err();
    assert_eq!(reason(&spent), "object.not_found", "the one use opened the watch");
    h.node(None, "fra-3", &[]).await;
    let placed = next(&mut stream).await.unwrap();
    assert_ne!(placed.convergence, now.convergence, "and the watch still hears each change");
    let revoke = pb::RevokeGrantRequest {
        mutation: mutation("revoke").into(),
        grant: issued.grant.as_option().unwrap().id.clone(),
        ..Default::default()
    };
    h.grants(h.root()).revoke_grant(revoke).await.unwrap();
    h.node(None, "fra-4", &[]).await;
    let e = next(&mut stream).await.unwrap_err();
    assert_eq!(reason(&e), "object.not_found", "until the link is revoked");
}
