// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations' layouts (question 2 of §10 of docs/11-surface-web.md): set by `org.write`,
//! inherited from the nearest organization that set one, checked item by item, read under the
//! read action of whatever the panel shows, and, for a landing no organization of the viewer's
//! names, the layout of the address: the root organization's, or at a portal its own.

use crate::domains::child;
use crate::links::in_an_hour;
use crate::portal_client::portal_of;
use crate::support::{Harness, detail, mutation, org_scope, reason};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::layout_item::Kind as K;

fn set(key: &str, org: &str, layout: Option<pb::Layout>, expected: u64) -> pb::SetLayoutRequest {
    pb::SetLayoutRequest {
        mutation: pb::Mutation {
            idempotency_key: key.to_owned(),
            expected_version: (expected > 0).then_some(expected),
            ..Default::default()
        }
        .into(),
        organization: org.to_owned(),
        layout: layout.into(),
        ..Default::default()
    }
}

fn item(kind: K) -> pb::LayoutItem {
    pb::LayoutItem { kind: Some(kind), ..Default::default() }
}

fn places() -> pb::LayoutItem {
    item(K::Places(Box::default()))
}

fn link(url: &str) -> pb::LayoutItem {
    let link = pb::LayoutLink { label: "Support".into(), url: url.into(), ..Default::default() };
    item(K::Link(Box::new(link)))
}

fn acme(name: &str) -> pb::Layout {
    let notice = pb::LayoutNotice {
        heading: "Maintenance tonight".into(),
        text: "From 22:00, for about an hour.".into(),
        ..Default::default()
    };
    pb::Layout {
        name: name.into(),
        items: vec![item(K::Notice(Box::new(notice))), places(), link("https://help.acme.example")],
        ..Default::default()
    }
}

fn of_org(org: &str) -> pb::GetLayoutRequest {
    let of = pb::Scope {
        kind: Some(pb::scope::Kind::Organization(org.to_owned())),
        ..Default::default()
    };
    pb::GetLayoutRequest { of: of.into(), ..Default::default() }
}

async fn layout_of(h: &Harness, org: &str) -> pb::GoverningLayout {
    let got = h.orgs(h.root()).get_layout(of_org(org)).await.unwrap().into_owned();
    got.governing.as_option().unwrap().clone()
}

fn named(g: &pb::GoverningLayout) -> &str {
    g.layout.as_option().map_or("", |l| l.name.as_str())
}

#[tokio::test]
async fn a_layout_is_set_inherited_cleared_and_shown_to_a_landing_no_one_names() {
    let h = Harness::new().await;
    let (root, orgs) = (h.root_org(), h.orgs(h.root()));
    let (_, secret) = h.bare_token().await;
    let nobody = h.orgs(h.config(Some(secret.expose())));
    let landing = || nobody.get_landing_layout(pb::GetLandingLayoutRequest::default());
    let first = landing().await.unwrap().into_owned();
    assert!(first.governing.as_option().unwrap().set_on.is_none(), "the panel's own at first");

    orgs.set_layout(set("l1", &root, Some(acme("Acme")), 0)).await.unwrap();
    let mine = layout_of(&h, &root).await;
    assert_eq!((mine.set_on.as_deref(), mine.own_version), (Some(root.as_str()), 1));
    assert_eq!(mine.layout.as_option().unwrap().items.len(), 3, "kept in its order");
    let shown = landing().await.unwrap().into_owned();
    let shown = shown.governing.as_option().unwrap();
    assert_eq!((named(shown), shown.own_version), ("Acme", 0), "no handle to change it");
    let unread = nobody.get_layout(of_org(&root)).await.unwrap_err();
    assert!(matches!(unread.code, ErrorCode::NotFound | ErrorCode::PermissionDenied));
    let link = pb::IssueGrantRequest {
        mutation: mutation("link").into(),
        actions: vec!["org.read".into()],
        scope: org_scope(&root).into(),
        conditions: pb::Conditions {
            max_uses: Some(5),
            expires_at: Some(in_an_hour()).into(),
            ..Default::default()
        }
        .into(),
        link_form: true,
        ..Default::default()
    };
    let link = h.grants(h.root()).issue_grant(link).await.unwrap().into_owned().link_token;
    let held = h.orgs(h.config(Some(&link)));
    let refused = held.get_landing_layout(pb::GetLandingLayoutRequest::default()).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::Unauthenticated, "a link has no landing");

    let reseller = child(&h, "reseller").await;
    let inherited = layout_of(&h, &reseller).await;
    assert_eq!((inherited.set_on.as_deref(), inherited.own_version), (Some(root.as_str()), 0));
    orgs.set_layout(set("l2", &reseller, Some(acme("Reseller")), 0)).await.unwrap();
    assert_eq!(named(&layout_of(&h, &reseller).await), "Reseller");
    orgs.set_layout(set("l3", &reseller, None, 1)).await.unwrap();
    let back = layout_of(&h, &reseller).await;
    assert_eq!((back.set_on.as_deref(), back.own_version), (Some(root.as_str()), 2));

    let stale = orgs.set_layout(set("l4", &root, Some(acme("Acme")), 0)).await.unwrap_err();
    assert_eq!(reason(&stale), "layout.moved");
    assert_eq!(h.effects("org.layout.set").await.len(), 3, "each change is an effect");
}

#[tokio::test]
async fn what_a_layout_may_not_hold_is_refused_by_its_item() {
    let h = Harness::new().await;
    let (root, orgs) = (h.root_org(), h.orgs(h.root()));
    let slot = item(K::Slot("ext.backup.status".into()));
    let cases = [
        (vec![link("https://help.acme.example")], "layout.items"),
        (vec![places(), places()], "layout.items"),
        (vec![places(), slot], "layout.items[1].slot"),
        (vec![places(), link("http://help.acme.example")], "layout.items[1].link.url"),
        (
            vec![places(), link("https://acme.example@elsewhere.example")],
            "layout.items[1].link.url",
        ),
        (vec![pb::LayoutItem::default(), places()], "layout.items[0]"),
    ];
    for (i, (items, field)) in cases.into_iter().enumerate() {
        let layout = pb::Layout { items, ..Default::default() };
        let e = orgs.set_layout(set(&format!("bad{i}"), &root, Some(layout), 0)).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument, "{field}");
        assert_eq!(detail(&e).field, field);
    }
    assert!(layout_of(&h, &root).await.set_on.is_none(), "nothing refused was kept");
}

#[tokio::test]
async fn at_a_portal_the_landing_is_its_organizations() {
    let h = Harness::new().await;
    let (root, orgs) = (h.root_org(), h.orgs(h.root()));
    let pinecone = child(&h, "pinecone").await;
    orgs.set_layout(set("root", &root, Some(acme("Acme")), 0)).await.unwrap();
    orgs.set_layout(set("pinecone", &pinecone, Some(acme("Pinecone")), 0)).await.unwrap();
    let portal = portal_of(&h, ("p", &pinecone), ("pinecone.example", "panel.pinecone.example"));
    let portal = portal.await;
    let (_, secret) = h.bare_token().await;
    let asked = pb::GetLandingLayoutRequest::default();
    let procedure = "korpis.v1.OrganizationService/GetLandingLayout";
    let there = portal.call_bearing(procedure, &asked, secret.expose()).await;
    let (there, _): (pb::GetLandingLayoutResponse, _) = there.unwrap();
    let there = there.governing.as_option().unwrap();
    assert_eq!((named(there), there.set_on.as_deref()), ("Pinecone", Some(pinecone.as_str())));
    let refused = portal.call_bearing::<pb::GetLandingLayoutResponse>(procedure, &asked, "nothing");
    assert_eq!(refused.await.unwrap_err(), reqwest::StatusCode::UNAUTHORIZED, "signed in only");
}
