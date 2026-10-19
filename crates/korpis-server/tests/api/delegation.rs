// SPDX-License-Identifier: AGPL-3.0-or-later

//! Three levels of delegation (§2 and §3.2 of docs/08-identity.md; the done-when of Phase 2 in §4
//! of docs/20-roadmap.md): the host delegates to a reseller's organization, the reseller makes a
//! sub-tenant and delegates to it, and the sub-tenant makes its customer and gives the customer
//! authority over one project. No level below the host holds the root grant, every hop is an
//! attenuation of the one above, and every widening is refused at the hop that tries it.

use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use connectrpc::client::ClientConfig;
use korpis_core::authz::{Kind, ObjectRef, Scope};
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::{principal, scope};
use uuid::Uuid;

/// Everything a tenant administrator of one level does.
pub(crate) const ADMIN: [&str; 5] =
    ["org.create", "org.read", "project.create", "project.read", "grant.read"];

/// Someone holding one grant: its id, and a client configuration that presents its token.
pub(crate) struct Holder {
    pub(crate) grant: String,
    pub(crate) config: ClientConfig,
    pub(crate) token: Uuid,
}

fn project_scope(id: &str) -> pb::Scope {
    let id = Id::parse(IdKind::Project, id).unwrap().uuid();
    scope::to_pb(&Scope::Object(ObjectRef::new(Kind::Project, id)))
}

pub(crate) fn delegation(depth: Option<u32>, actions: &[&str]) -> pb::DelegationPolicy {
    pb::DelegationPolicy {
        allowed: depth.is_some_and(|d| d > 0),
        max_depth: depth,
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        ..Default::default()
    }
}

/// What `from` asks for when it issues `actions` on `scope` to `to`, with `policy`.
fn issue(
    from: &str,
    to: Uuid,
    (actions, scope): (&[&str], pb::Scope),
    policy: pb::DelegationPolicy,
) -> pb::IssueGrantRequest {
    pb::IssueGrantRequest {
        mutation: mutation(&format!("to-{to}-{}", Uuid::now_v7())).into(),
        parent: from.to_owned(),
        subject: principal::to_pb(&Principal::Token(to)).into(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope: scope.into(),
        delegation: policy.into(),
        ..Default::default()
    }
}

/// A new token given `actions` on `scope` by `parent`, which presents `config`.
pub(crate) async fn delegate(
    h: &Harness,
    (parent, config): (&str, ClientConfig),
    (actions, scope): (&[&str], pb::Scope),
    policy: pb::DelegationPolicy,
) -> Holder {
    let (token, secret) = h.bare_token().await;
    let issued = h
        .grants(config)
        .issue_grant(issue(parent, token, (actions, scope), policy))
        .await
        .unwrap()
        .into_owned();
    let grant = issued.grant.as_option().unwrap().id.clone();
    Holder { grant, config: h.config(Some(secret.expose())), token }
}

/// An organization named `name` under `parent`, made by `by`; its id.
pub(crate) async fn organization(h: &Harness, by: &Holder, parent: &str, name: &str) -> String {
    let made = h
        .orgs(by.config.clone())
        .create_organization(pb::CreateOrganizationRequest {
            mutation: mutation(&format!("org-{name}")).into(),
            name: name.into(),
            parent: Some(parent.to_owned()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    made.organization.as_option().unwrap().id.clone()
}

/// The code `by` is refused with when it issues as `ask` says.
async fn refused(h: &Harness, by: &Holder, ask: pb::IssueGrantRequest) -> ErrorCode {
    h.grants(by.config.clone()).issue_grant(ask).await.unwrap_err().code
}

/// The host's reseller, and the sub-tenant the reseller made and delegated to, after checking
/// that the reseller can make nothing beside itself nor give a wider scope than its own.
pub(crate) async fn reseller_and_sub_tenant(harness: &Harness) -> (Holder, Holder, String) {
    let host = Holder { grant: harness.root_grant(), config: harness.root(), token: Uuid::nil() };
    let root = harness.root_org();
    let org = organization(harness, &host, &root, "reseller").await;
    let ask = (&ADMIN[..], org_scope(&org));
    let reseller =
        delegate(harness, (&host.grant, harness.root()), ask, delegation(Some(3), &ADMIN)).await;

    let sub = organization(harness, &reseller, &org, "sub").await;
    let beside = pb::CreateOrganizationRequest {
        mutation: mutation("beside").into(),
        name: "beside".into(),
        parent: Some(root.clone()),
        ..Default::default()
    };
    let orgs = harness.orgs(reseller.config.clone());
    let refusal = orgs.create_organization(beside).await.unwrap_err();
    assert_eq!(refusal.code, ErrorCode::NotFound, "the host's organization does not exist for it");
    let wider =
        issue(&reseller.grant, reseller.token, (&ADMIN, org_scope(&root)), delegation(None, &[]));
    let code = refused(harness, &reseller, wider).await;
    assert_eq!(code, ErrorCode::InvalidArgument, "no wider scope");
    let passed = delegation(Some(2), &["project.read", "org.read"]);
    let from = (reseller.grant.as_str(), reseller.config.clone());
    let sub_tenant = delegate(harness, from, (&ADMIN, org_scope(&sub)), passed).await;
    (reseller, sub_tenant, sub)
}

#[tokio::test]
async fn a_reseller_delegates_to_a_sub_tenant_who_delegates_to_a_customer() {
    let harness = Harness::new().await;
    let (reseller, sub_tenant, sub) = reseller_and_sub_tenant(&harness).await;

    // The sub-tenant makes its customer and a project in it, and gives the customer that one
    // project; it cannot give more depth or more actions than it was given.
    let customer_org = organization(&harness, &sub_tenant, &sub, "customer").await;
    let made = harness
        .projects(sub_tenant.config.clone())
        .create_project(pb::CreateProjectRequest {
            mutation: mutation("shop").into(),
            organization: customer_org.clone(),
            name: "shop".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let shop = made.project.as_option().unwrap().id.clone();
    let read = (&["project.read"][..], project_scope(&shop));
    let deeper = issue(
        &sub_tenant.grant,
        sub_tenant.token,
        read.clone(),
        delegation(Some(2), &["project.read"]),
    );
    let code = refused(&harness, &sub_tenant, deeper).await;
    assert_eq!(code, ErrorCode::InvalidArgument, "no deeper delegation");
    let create = (&["org.create"][..], project_scope(&shop));
    let more = issue(&sub_tenant.grant, sub_tenant.token, create, delegation(None, &[]));
    let code = refused(&harness, &sub_tenant, more).await;
    assert_eq!(code, ErrorCode::InvalidArgument, "only what it may pass on");
    let from = (sub_tenant.grant.as_str(), sub_tenant.config.clone());
    let customer = delegate(&harness, from, read.clone(), delegation(None, &[])).await;

    // The customer reads its project and nothing above it, and passes nothing on.
    let get = pb::GetProjectRequest { id: shop.clone(), ..Default::default() };
    harness.projects(customer.config.clone()).get_project(get).await.expect("its own project");
    let above = pb::GetOrganizationRequest { id: customer_org, ..Default::default() };
    let refusal = harness.orgs(customer.config.clone()).get_organization(above).await.unwrap_err();
    assert_eq!(refusal.code, ErrorCode::NotFound, "the organization above it is not there for it");
    let onward = issue(&customer.grant, customer.token, read, delegation(None, &[]));
    let code = refused(&harness, &customer, onward).await;
    assert_eq!(code, ErrorCode::InvalidArgument, "it may not delegate");

    // Each organization was made under the grant of the level above, never the host's.
    let made_under: Vec<Option<Uuid>> =
        harness.effects("org.create").await.into_iter().map(|(_, g)| g).collect();
    let grant = |id: &str| Some(Id::parse(IdKind::Grant, id).unwrap().uuid());
    let by = |holder: &Holder| made_under.iter().filter(|g| **g == grant(&holder.grant)).count();
    assert_eq!(by(&reseller), 1, "the sub-tenant's organization, by the reseller");
    assert_eq!(by(&sub_tenant), 1, "the customer's organization, by the sub-tenant");
}
