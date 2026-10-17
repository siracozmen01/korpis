// SPDX-License-Identifier: AGPL-3.0-or-later

//! A secret field's value is read back only with `workload.secret.read` (§5.2 of
//! docs/09-recipes.md, Finding 84 of docs/23-walkthroughs.md): every read that returns an intent
//! withholds it from anyone else and names it, and such a caller keeps it across a change it
//! declares without ever learning it.

use crate::recipes::{PAPER, body, change, create, project, push_paper, push_paper_like, text};
use crate::support::{Harness, detail};
use connectrpc::ErrorCode;
use connectrpc::client::HttpClient;
use korpis_proto::korpis::v1 as pb;

const SECRET: &str = "hunter2-rcon";

/// A workload whose RCON password is set, and its id.
async fn guarded(h: &Harness) -> (String, String) {
    let repo = push_paper().await;
    let project = project(h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create(
            "d",
            &project,
            "mc",
            body(&repo, &[("rcon_password", text(SECRET))]),
        ))
        .await
        .unwrap()
        .into_owned();
    (repo, declared.workload.as_option().unwrap().id.clone())
}

async fn head(reads: &pb::WorkloadServiceClient<HttpClient>, w: &str) -> pb::Intent {
    let got = reads
        .get_workload(pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    got.status.as_option().unwrap().head.as_option().unwrap().clone()
}

#[tokio::test]
async fn a_secret_is_withheld_from_every_read_without_the_secret_read_action() {
    let h = Harness::new().await;
    let (_, w) = guarded(&h).await;
    let (reader, secret) = h.bare_token().await;
    h.issue_to(reader, "reader", &["workload.read"]).await;
    let reads = h.workloads(h.config(Some(secret.expose())));

    let shown = head(&reads, &w).await;
    assert!(!shown.body.as_option().unwrap().config.contains_key("rcon_password"));
    assert_eq!(shown.withheld, vec!["rcon_password".to_owned()], "named, so a client knows");
    let listed = reads
        .list_intents(pb::ListIntentsRequest { workload: w.clone(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    let all = format!("{:?}", listed.intents);
    assert!(
        !all.contains(SECRET) && listed.intents.iter().all(|i| !i.withheld.is_empty()),
        "{all}"
    );

    let (trusted, key) = h.bare_token().await;
    h.issue_to(trusted, "trusted", &["workload.read", "workload.secret.read"]).await;
    let full = head(&h.workloads(h.config(Some(key.expose()))), &w).await;
    assert!(full.withheld.is_empty());
    assert!(format!("{:?}", full.body).contains(SECRET), "read back with the action");
}

#[tokio::test]
async fn a_caller_that_may_not_read_a_secret_keeps_it_across_a_change() {
    let h = Harness::new().await;
    let (repo, w) = guarded(&h).await;
    let (token, secret) = h.bare_token().await;
    h.issue_to(token, "writer", &["workload.read", "workload.write"]).await;
    let writes = h.workloads(h.config(Some(secret.expose())));
    let version = head(&writes, &w).await.version;

    let mut request = change("c1", &w, version, body(&repo, &[("motd", text("hello"))]));
    request.keep = vec!["rcon_password".to_owned()];
    let declared = writes.declare_intent(request).await.unwrap().into_owned();
    let answered = declared.intent.as_option().unwrap();
    assert_eq!(answered.withheld, vec!["rcon_password".to_owned()], "nor does the answer show it");
    let now = head(&h.workloads(h.root()), &w).await;
    assert!(format!("{:?}", now.body).contains(SECRET), "kept as it was");
    assert!(format!("{:?}", now.body).contains("hello"));

    let mut both = change("c2", &w, now.version, body(&repo, &[("rcon_password", text("x"))]));
    both.keep = vec!["rcon_password".to_owned()];
    let e = writes.declare_intent(both).await.unwrap_err();
    assert_eq!((e.code, detail(&e).field.as_str()), (ErrorCode::InvalidArgument, "keep"));
}

#[tokio::test]
async fn a_kept_secret_cannot_be_carried_into_a_recipe_that_does_not_keep_it_secret() {
    let h = Harness::new().await;
    let (_, w) = guarded(&h).await;
    let open = PAPER.replace(
        "type: string, secret: true, permission: operator",
        "type: string, permission: operator",
    );
    assert_ne!(open, PAPER, "the example marks it secret");
    let open = push_paper_like(&open).await;
    let (token, secret) = h.bare_token().await;
    h.issue_to(token, "writer", &["workload.read", "workload.write"]).await;
    let writes = h.workloads(h.config(Some(secret.expose())));
    let version = head(&writes, &w).await.version;

    let mut request = change("c1", &w, version, body(&open, &[]));
    request.keep = vec!["rcon_password".to_owned()];
    let e = writes.declare_intent(request).await.unwrap_err();
    assert!(matches!(e.code, ErrorCode::PermissionDenied | ErrorCode::NotFound), "{e:?}");
    let shown = head(&writes, &w).await;
    assert_eq!(shown.version, version, "nothing was declared");

    let root = h.workloads(h.root());
    let mut allowed = change("c2", &w, version, body(&open, &[]));
    allowed.keep = vec!["rcon_password".to_owned()];
    root.declare_intent(allowed).await.unwrap();
    assert!(format!("{:?}", head(&root, &w).await.body).contains(SECRET), "who may read it may");
}
