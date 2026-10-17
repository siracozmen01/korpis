// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations' themes (§7 of docs/11-surface-web.md): set by `org.write`, inherited from the
//! nearest organization that set one, checked field by field, worn by the sign-in page before
//! anyone signs in, and read under the read action of whatever the panel shows.

use crate::support::{Harness, detail, mutation, reason};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

fn set(key: &str, org: &str, theme: Option<pb::Theme>, expected: u64) -> pb::SetThemeRequest {
    pb::SetThemeRequest {
        mutation: pb::Mutation {
            idempotency_key: key.to_owned(),
            expected_version: (expected > 0).then_some(expected),
            ..Default::default()
        }
        .into(),
        organization: org.to_owned(),
        theme: theme.into(),
        ..Default::default()
    }
}

fn acme() -> pb::Theme {
    pb::Theme {
        product_name: "Acme Hosting".into(),
        logo: pb::Logo {
            media_type: "image/svg+xml".into(),
            data: b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(),
            ..Default::default()
        }
        .into(),
        light: pb::ThemeColors { accent: "#1F5FBF".into(), ..Default::default() }.into(),
        sign_in_text: "Sign in to manage your servers.".into(),
        stylesheet: ".brand { font-weight: 600 }".into(),
        ..Default::default()
    }
}

fn of_org(org: &str) -> pb::GetThemeRequest {
    let of = pb::Scope {
        kind: Some(pb::scope::Kind::Organization(org.to_owned())),
        ..Default::default()
    };
    pb::GetThemeRequest { of: of.into(), ..Default::default() }
}

async fn theme_of(h: &Harness, org: &str) -> pb::GoverningTheme {
    let got = h.orgs(h.root()).get_theme(of_org(org)).await.unwrap().into_owned();
    got.governing.as_option().unwrap().clone()
}

async fn sign_in(h: &Harness) -> pb::GoverningTheme {
    let got = pb::ServerInfoServiceClient::new(h.http.clone(), h.config(None))
        .get_sign_in_theme(pb::GetSignInThemeRequest::default())
        .await
        .unwrap()
        .into_owned();
    got.governing.as_option().unwrap().clone()
}

async fn child(h: &Harness, name: &str) -> String {
    let created = h
        .orgs(h.root())
        .create_organization(pb::CreateOrganizationRequest {
            mutation: mutation(&format!("org-{name}")).into(),
            name: name.into(),
            parent: Some(h.root_org()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    created.organization.as_option().unwrap().id.clone()
}

#[tokio::test]
async fn a_theme_is_set_inherited_cleared_and_worn_by_the_sign_in_page() {
    let h = Harness::new().await;
    let (root, orgs) = (h.root_org(), h.orgs(h.root()));
    assert!(sign_in(&h).await.set_on.is_none(), "the panel's own before any is set");

    orgs.set_theme(set("t1", &root, Some(acme()), 0)).await.unwrap();
    let mine = theme_of(&h, &root).await;
    assert_eq!((mine.set_on.as_deref(), mine.own_version), (Some(root.as_str()), 1));
    let theme = mine.theme.as_option().unwrap();
    assert_eq!(theme.light.as_option().unwrap().accent, "#1f5fbf", "kept lower case");
    let page = sign_in(&h).await;
    assert_eq!(page.theme.as_option().unwrap().product_name, "Acme Hosting");
    assert_eq!(page.own_version, 0, "the sign-in page's copy is no handle to change it");

    let reseller = child(&h, "reseller").await;
    let inherited = theme_of(&h, &reseller).await;
    assert_eq!((inherited.set_on.as_deref(), inherited.own_version), (Some(root.as_str()), 0));
    let own = pb::Theme { product_name: "Reseller".into(), ..Default::default() };
    orgs.set_theme(set("t2", &reseller, Some(own), 0)).await.unwrap();
    assert_eq!(theme_of(&h, &reseller).await.set_on.as_deref(), Some(reseller.as_str()));
    orgs.set_theme(set("t3", &reseller, None, 1)).await.unwrap();
    let back = theme_of(&h, &reseller).await;
    assert_eq!((back.set_on.as_deref(), back.own_version), (Some(root.as_str()), 2));

    let stale = orgs.set_theme(set("t4", &root, Some(acme()), 0)).await.unwrap_err();
    assert_eq!(reason(&stale), "theme.moved");
    assert_eq!(h.effects("org.theme.set").await.len(), 3, "each change is an effect");
}

#[tokio::test]
async fn what_a_theme_may_not_hold_is_refused_by_its_field() {
    let h = Harness::new().await;
    let (root, orgs) = (h.root_org(), h.orgs(h.root()));
    let cases = [
        (
            pb::Theme { stylesheet: "@import url(https://evil.example/x.css);".into(), ..acme() },
            "theme.stylesheet",
        ),
        (
            pb::Theme {
                dark: pb::ThemeColors { text: "red".into(), ..Default::default() }.into(),
                ..acme()
            },
            "theme.dark.text",
        ),
        (
            pb::Theme {
                logo: pb::Logo {
                    media_type: "image/png".into(),
                    data: b"<svg/>".to_vec(),
                    ..Default::default()
                }
                .into(),
                ..acme()
            },
            "theme.logo.data",
        ),
    ];
    for (i, (theme, field)) in cases.into_iter().enumerate() {
        let e = orgs.set_theme(set(&format!("bad{i}"), &root, Some(theme), 0)).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument, "{field}");
        assert_eq!(detail(&e).field, field);
    }
    assert!(theme_of(&h, &root).await.set_on.is_none(), "nothing refused was kept");
}

#[tokio::test]
async fn a_theme_is_read_under_the_read_action_of_what_is_shown() {
    let h = Harness::new().await;
    let root = h.root_org();
    h.orgs(h.root()).set_theme(set("t1", &root, Some(acme()), 0)).await.unwrap();
    let (token, secret) = h.bare_token().await;
    let none = h.orgs(h.config(Some(secret.expose()))).get_theme(of_org(&root)).await.unwrap_err();
    assert!(matches!(none.code, ErrorCode::NotFound | ErrorCode::PermissionDenied), "{none:?}");
    h.issue_to(token, "reader", &["org.read"]).await;
    let read = h.orgs(h.config(Some(secret.expose()))).get_theme(of_org(&root)).await.unwrap();
    let governing = read.into_owned().governing.as_option().unwrap().clone();
    assert_eq!(governing.theme.as_option().unwrap().product_name, "Acme Hosting");
}
