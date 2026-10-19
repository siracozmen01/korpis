// SPDX-License-Identifier: AGPL-3.0-or-later

//! Page tokens (§8 of docs/10-api.md), on a real control plane: a token is opaque, naming no
//! row, and following it walks the whole list once; a token one list gave is refused by another,
//! and one no list gave is refused outright.

use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

fn listing(parent: &str, page_token: String) -> pb::ListOrganizationsRequest {
    pb::ListOrganizationsRequest {
        parent: Some(parent.to_owned()),
        page: pb::PageRequest { page_size: 1, page_token, ..Default::default() }.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_page_token_names_nothing_walks_the_list_once_and_is_refused_elsewhere() {
    let harness = Harness::new().await;
    let orgs = harness.orgs(harness.root());
    let root = harness.root_org();
    for name in ["one", "two", "three"] {
        let made = pb::CreateOrganizationRequest {
            mutation: mutation(name).into(),
            name: name.into(),
            parent: Some(root.clone()),
            ..Default::default()
        };
        orgs.create_organization(made).await.unwrap();
    }
    let (mut names, mut tokens) = (Vec::new(), Vec::new());
    let mut token = String::new();
    loop {
        let page = orgs.list_organizations(listing(&root, token)).await.unwrap().into_owned();
        names.extend(page.organizations.into_iter().map(|o| o.name));
        token = page.page.as_option().map(|p| p.next_page_token.clone()).unwrap_or_default();
        if token.is_empty() {
            break;
        }
        tokens.push(token.clone());
    }
    names.sort();
    assert_eq!(names, ["one", "three", "two"], "each once");
    assert!(tokens.iter().all(|t| !t.starts_with("org_")), "a token names no row: {tokens:?}");

    let projects = pb::ListProjectsRequest {
        organization: root.clone(),
        page: pb::PageRequest { page_token: tokens[0].clone(), ..Default::default() }.into(),
        ..Default::default()
    };
    let elsewhere = harness.projects(harness.root()).list_projects(projects).await.unwrap_err();
    assert_eq!(
        elsewhere.code,
        ErrorCode::InvalidArgument,
        "an organization token pages no projects"
    );
    let forged = orgs.list_organizations(listing(&root, "org_0".into())).await.unwrap_err();
    assert_eq!(forged.code, ErrorCode::InvalidArgument, "a token no list gave");
}
