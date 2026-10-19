// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grant templates on a real control plane (§4 of docs/08-identity.md): a reseller defines a
//! Moderator, which is offered to its customer and not to the host above it; the customer reads
//! it where it is offered and can neither edit it nor learn it exists by its identifier; an edit
//! made against a version that has moved on is a conflict, and every version stays readable;
//! retiring it offers it nowhere and frees its name. A template that does not say whether its
//! grants delegate, or delegates more than it permits, is refused before anything is written.

use crate::delegation::{Holder, delegate, delegation, organization};
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

const RESELLER: [&str; 4] = ["template.write", "org.read", "org.create", "grant.read"];

fn moderator(name: &str, actions: &[&str]) -> pb::GrantTemplateBody {
    pb::GrantTemplateBody {
        name: name.into(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope_pattern: pb::ScopePattern {
            kind: pb::ObjectKind::OBJECT_KIND_PROJECT.into(),
            ..Default::default()
        }
        .into(),
        delegation: delegation(Some(1), &["workload.restart"]).into(),
        ..Default::default()
    }
}

fn at(org: &str) -> pb::ObjectRef {
    pb::ObjectRef {
        kind: pb::ObjectKind::OBJECT_KIND_ORGANIZATION.into(),
        id: org.to_owned(),
        ..Default::default()
    }
}

async fn offered(harness: &Harness, by: &Holder, org: &str) -> Vec<String> {
    let request = pb::ListGrantTemplatesRequest {
        of: Some(pb::__buffa::oneof::list_grant_templates_request::Of::At(Box::new(at(org)))),
        ..Default::default()
    };
    let listed = harness.grants(by.config.clone()).list_grant_templates(request).await.unwrap();
    listed.into_owned().templates.into_iter().map(|t| t.name).collect()
}

/// The host, a reseller holding `RESELLER` on its organization, and the reseller's customer
/// holding `org.read` on its own.
async fn tenants(harness: &Harness) -> (Holder, Holder, Holder, String, String) {
    let host = Holder { grant: harness.root_grant(), config: harness.root(), token: Uuid::nil() };
    let reseller_org = organization(harness, &host, &harness.root_org(), "reseller").await;
    let ask = (&RESELLER[..], org_scope(&reseller_org));
    let policy = delegation(Some(2), &["org.read"]);
    let reseller = delegate(harness, (&host.grant, harness.root()), ask, policy).await;
    let customer_org = organization(harness, &reseller, &reseller_org, "customer").await;
    let from = (reseller.grant.as_str(), reseller.config.clone());
    let read = (&["org.read"][..], org_scope(&customer_org));
    let customer = delegate(harness, from, read, delegation(None, &[])).await;
    (host, reseller, customer, reseller_org, customer_org)
}

#[tokio::test]
async fn a_resellers_moderator_is_offered_below_it_versioned_and_retired() {
    let harness = Harness::new().await;
    let (host, reseller, customer, reseller_org, customer_org) = tenants(&harness).await;
    let grants = harness.grants(reseller.config.clone());
    let create = |key: &str, body: pb::GrantTemplateBody| pb::CreateGrantTemplateRequest {
        mutation: mutation(key).into(),
        organization: reseller_org.clone(),
        template: body.into(),
        ..Default::default()
    };
    let actions = ["workload.console.read", "workload.restart"];
    let made = grants.create_grant_template(create("m1", moderator("Moderator", &actions))).await;
    let made = made.unwrap().into_owned().template.into_option().unwrap();
    assert_eq!((made.version, made.organization.as_str()), (1, reseller_org.as_str()));
    let twice = grants.create_grant_template(create("m2", moderator("Moderator", &actions))).await;
    assert_eq!(twice.unwrap_err().code, ErrorCode::Aborted, "one live Moderator");

    // Offered at the customer, beneath its owner, and not at the host above it.
    assert_eq!(offered(&harness, &customer, &customer_org).await, ["Moderator"]);
    assert!(offered(&harness, &host, &harness.root_org()).await.is_empty(), "never upward");
    let tenant = harness.grants(customer.config.clone());
    let get = |at: Option<pb::ObjectRef>, version: Option<u64>| pb::GetGrantTemplateRequest {
        id: made.id.clone(),
        version,
        at: at.into(),
        ..Default::default()
    };
    tenant.get_grant_template(get(Some(at(&customer_org)), None)).await.expect("where offered");
    let unseen = tenant.get_grant_template(get(None, None)).await.unwrap_err();
    assert_eq!(unseen.code, ErrorCode::NotFound, "its owner is not there for the customer");
    let edit =
        |key: &str, expected: u64, body: pb::GrantTemplateBody| pb::UpdateGrantTemplateRequest {
            mutation: pb::Mutation { expected_version: Some(expected), ..mutation(key) }.into(),
            id: made.id.clone(),
            template: body.into(),
            ..Default::default()
        };
    let wider = moderator("Moderator", &["workload.restart", "workload.exec"]);
    let theirs = tenant.update_grant_template(edit("t1", 1, wider.clone())).await.unwrap_err();
    assert_eq!(theirs.code, ErrorCode::NotFound, "nor is a template it cannot edit");

    // An edit is a new version; one made against the old version is a conflict, and the old
    // version is still what it was.
    let v2 = grants.update_grant_template(edit("e1", 1, wider.clone())).await.unwrap();
    assert_eq!(v2.into_owned().template.into_option().unwrap().version, 2);
    let stale = grants.update_grant_template(edit("e2", 1, wider)).await.unwrap_err();
    assert_eq!(stale.code, ErrorCode::Aborted, "made against version 1");
    let first = grants.get_grant_template(get(None, Some(1))).await.unwrap().into_owned();
    assert_eq!(first.template.into_option().unwrap().actions, actions, "version 1 as it was");

    retired(&harness, (&reseller, &customer), (&made.id, &reseller_org, &customer_org)).await;
}

/// Retires the Moderator: offered nowhere since, still the reseller's to read, its name free
/// again; and every change was recorded once.
async fn retired(
    harness: &Harness,
    (reseller, customer): (&Holder, &Holder),
    (id, reseller_org, customer_org): (&str, &str, &str),
) {
    let grants = harness.grants(reseller.config.clone());
    let retire = pb::RetireGrantTemplateRequest {
        mutation: mutation("r1").into(),
        id: id.to_owned(),
        ..Default::default()
    };
    let retired = grants.retire_grant_template(retire).await.unwrap().into_owned();
    assert!(retired.template.into_option().unwrap().retired_at.as_option().is_some());
    assert!(offered(harness, customer, customer_org).await.is_empty(), "offered nowhere");
    let get = pb::GetGrantTemplateRequest {
        id: id.to_owned(),
        at: Some(at(customer_org)).into(),
        ..Default::default()
    };
    let tenant = harness.grants(customer.config.clone());
    assert_eq!(tenant.get_grant_template(get).await.unwrap_err().code, ErrorCode::NotFound);
    let owned = pb::ListGrantTemplatesRequest {
        of: Some(pb::__buffa::oneof::list_grant_templates_request::Of::Owned(
            reseller_org.to_owned(),
        )),
        ..Default::default()
    };
    let own = grants.list_grant_templates(owned).await.unwrap().into_owned().templates;
    assert_eq!(own.len(), 1, "its owner still reads it");
    let again = pb::CreateGrantTemplateRequest {
        mutation: mutation("m3").into(),
        organization: reseller_org.to_owned(),
        template: moderator("Moderator", &["workload.restart"]).into(),
        ..Default::default()
    };
    grants.create_grant_template(again).await.expect("its name is free again");
    assert_eq!(harness.effects("template.create").await.len(), 2);
    assert_eq!(harness.effects("template.update").await.len(), 1);
    assert_eq!(harness.effects("template.retire").await.len(), 1);
}

#[tokio::test]
async fn a_template_says_what_it_delegates_and_is_made_only_by_its_owner() {
    let harness = Harness::new().await;
    let (_host, reseller, customer, reseller_org, customer_org) = tenants(&harness).await;
    let grants = harness.grants(reseller.config.clone());
    let create = |org: &str, body: pb::GrantTemplateBody| pb::CreateGrantTemplateRequest {
        mutation: mutation(&format!("c-{}", Uuid::now_v7())).into(),
        organization: org.to_owned(),
        template: body.into(),
        ..Default::default()
    };
    let refused = |e: connectrpc::ConnectError| (e.code, e.message.unwrap_or_default());
    let mut silent = moderator("Silent", &["workload.restart"]);
    silent.delegation = None.into();
    let (code, message) =
        refused(grants.create_grant_template(create(&reseller_org, silent)).await.unwrap_err());
    assert_eq!(code, ErrorCode::InvalidArgument);
    assert!(message.contains("delegate"), "{message}");
    let mut more = moderator("More", &["workload.console.read"]);
    more.delegation = delegation(Some(1), &["workload.exec"]).into();
    let (code, _) =
        refused(grants.create_grant_template(create(&reseller_org, more)).await.unwrap_err());
    assert_eq!(code, ErrorCode::InvalidArgument, "delegates only what it permits");

    // The customer reads its organization and holds no template.write there.
    let theirs = harness.grants(customer.config.clone());
    let own = create(&customer_org, moderator("Own", &["workload.restart"]));
    let denied = theirs.create_grant_template(own).await.unwrap_err();
    assert_eq!(denied.code, ErrorCode::PermissionDenied);
}
