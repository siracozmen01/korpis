// SPDX-License-Identifier: AGPL-3.0-or-later

//! Repositories (§5.3 of docs/06-storage.md): made under `repository.write` with a policy that
//! keeps something, at a prefix in a backup target, with the key held by the control plane, or
//! under tenant custody only when it names the tenant's key (`tenant_keys.rs` makes one); a name
//! once per organization; read
//! under `repository.read`, and listed for a project with the organization's own.

use crate::recipes::project;
use crate::support::{Harness, mutation, reason};
use crate::volumes::holding;
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

fn create(h: &Harness, key: &str, name: &str) -> pb::CreateRepositoryRequest {
    pb::CreateRepositoryRequest {
        mutation: mutation(key).into(),
        organization: h.root_org(),
        name: name.into(),
        provider: "s3".into(),
        prefix: "korpis/".into(),
        retention: pb::RetentionPolicy { keep_daily: 7, ..Default::default() }.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_repository_is_made_read_and_listed_as_its_grants_allow() {
    let h = Harness::new().await;
    let repos = h.repositories(h.root());
    let made = repos.create_repository(create(&h, "r1", "nightly")).await.unwrap().into_owned();
    let r = made.repository.into_option().unwrap();
    assert!(r.id.starts_with("rep_"));
    assert_eq!(r.key_custody.as_known(), Some(pb::KeyCustody::KEY_CUSTODY_OPERATOR), "the default");
    let again = repos.create_repository(create(&h, "r1", "nightly")).await.unwrap().into_owned();
    assert_eq!(again.repository.into_option().unwrap().id, r.id, "replayed");
    let e = repos.create_repository(create(&h, "r2", "nightly")).await.unwrap_err();
    assert_eq!(reason(&e), "repository.name_taken");

    let p = project(&h, &h.root_org(), "games").await;
    let scoped = pb::CreateRepositoryRequest {
        project: Some(p.clone()),
        prefix: "games/".into(),
        ..create(&h, "r3", "games-only")
    };
    repos.create_repository(scoped).await.unwrap();
    let list = |project: Option<String>| pb::ListRepositoriesRequest {
        organization: h.root_org(),
        project,
        ..Default::default()
    };
    let names = |l: pb::ListRepositoriesResponse| {
        l.repositories.into_iter().map(|r| r.name).collect::<Vec<_>>()
    };
    let all = repos.list_repositories(list(None)).await.unwrap().into_owned();
    assert_eq!(names(all), ["nightly", "games-only"]);
    let other = project(&h, &h.root_org(), "web").await;
    let for_web = repos.list_repositories(list(Some(other))).await.unwrap().into_owned();
    assert_eq!(names(for_web), ["nightly"], "a project sees its own and the organization's");

    let reader = holding(&h, &["repository.read", "org.read"]).await;
    let got = pb::GetRepositoryRequest { id: r.id.clone(), ..Default::default() };
    assert_eq!(
        h.repositories(reader.clone())
            .get_repository(got)
            .await
            .unwrap()
            .into_owned()
            .repository
            .into_option()
            .unwrap()
            .name,
        "nightly"
    );
    let e = h.repositories(reader).create_repository(create(&h, "r4", "mine")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn what_a_repository_cannot_be_is_refused_with_its_reason() {
    let h = Harness::new().await;
    let repos = h.repositories(h.root());
    let tenant = pb::CreateRepositoryRequest {
        key_custody: pb::KeyCustody::KEY_CUSTODY_TENANT.into(),
        ..create(&h, "t", "theirs")
    };
    let e = repos.create_repository(tenant).await.unwrap_err();
    assert_eq!(reason(&e), "request.invalid", "tenant custody names the tenant's key");
    let nothing = pb::CreateRepositoryRequest {
        retention: pb::RetentionPolicy::default().into(),
        ..create(&h, "n", "none")
    };
    assert_eq!(
        repos.create_repository(nothing).await.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    repos.create_repository(create(&h, "base", "base")).await.unwrap();
    let workload_place = "korpis/workloads/0193a5b0-0000-7000-8000-000000000000/x/";
    for (key, prefix) in [("in", workload_place), ("same", "korpis/")] {
        let overlapping =
            pb::CreateRepositoryRequest { prefix: prefix.into(), ..create(&h, key, key) };
        let refused = repos.create_repository(overlapping).await.unwrap_err();
        assert_eq!(
            reason(&refused),
            "repository.prefix_overlaps",
            "{prefix}: one prune would empty the other"
        );
    }
    for (key, prefix) in [("beside", "korpis-other/"), ("inner", "korpis/inner/")] {
        let apart = pb::CreateRepositoryRequest { prefix: prefix.into(), ..create(&h, key, key) };
        repos.create_repository(apart).await.unwrap();
    }
    let around = pb::CreateRepositoryRequest {
        prefix: "korpis/inner/workloads/".into(),
        ..create(&h, "around", "around")
    };
    let refused = repos.create_repository(around).await.unwrap_err();
    assert_eq!(reason(&refused), "repository.prefix_overlaps", "the other way round too");
    let elsewhere =
        pb::CreateRepositoryRequest { provider: "b2".into(), ..create(&h, "else", "elsewhere") };
    repos.create_repository(elsewhere).await.unwrap();
    for prefix in ["", "korpis", "../up/", "/abs/"] {
        let bad =
            pb::CreateRepositoryRequest { prefix: prefix.into(), ..create(&h, prefix, "bad") };
        assert_eq!(
            repos.create_repository(bad).await.unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{prefix:?}"
        );
    }
}
