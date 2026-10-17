// SPDX-License-Identifier: AGPL-3.0-or-later

//! Share links (§6.1 and §6.2 of docs/08-identity.md): a link-form grant is answered once as a
//! link, which the gateway accepts as a credential on its own and nothing else accepts; each call
//! it authorizes spends a use, and nothing it is used for goes uncounted; it must expire, limit
//! its uses, name one object, and delegate nothing; a session opened through it spends one use and
//! its renewals none; and revoking it ends it at the next call.

use crate::support::{Harness, detail, mutation, org_scope, reason};
use connectrpc::ErrorCode;
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::subject::Kind;
use korpis_server::api::wire::principal;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// An hour from now, as a link's expiry.
pub fn in_an_hour() -> buffa_types::google::protobuf::Timestamp {
    let at = SystemTime::now() + Duration::from_secs(3600);
    let seconds = i64::try_from(at.duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap();
    buffa_types::google::protobuf::Timestamp { seconds, ..Default::default() }
}

/// A link to read the root organization, `uses` times.
fn link(h: &Harness, key: &str, actions: &[&str], uses: u64) -> pb::IssueGrantRequest {
    pb::IssueGrantRequest {
        mutation: mutation(key).into(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope: org_scope(&h.root_org()).into(),
        conditions: pb::Conditions {
            expires_at: Some(in_an_hour()).into(),
            max_uses: Some(uses),
            ..Default::default()
        }
        .into(),
        link_form: true,
        ..Default::default()
    }
}

async fn read_org(h: &Harness, credential: &str) -> Result<(), connectrpc::ConnectError> {
    let r = pb::GetOrganizationRequest { id: h.root_org(), ..Default::default() };
    h.orgs(h.config(Some(credential))).get_organization(r).await.map(|_| ())
}

#[tokio::test]
async fn a_link_is_answered_once_held_by_its_bearer_and_spent_by_each_call() {
    let h = Harness::new().await;
    let request = link(&h, "share", &["org.read"], 2);
    let issued = h.grants(h.root()).issue_grant(request.clone()).await.unwrap().into_owned();
    let grant = issued.grant.as_option().unwrap().clone();
    assert!(issued.link_token.starts_with("korpislk_"), "the link, once");
    assert!(grant.link_form);
    let holder = grant.subject.as_option().and_then(|s| s.kind.clone());
    assert_eq!(holder, Some(Kind::Link(grant.id.clone())), "held by whoever holds the link");
    let replayed = h.grants(h.root()).issue_grant(request).await.unwrap().into_owned();
    assert!(replayed.link_token.is_empty(), "a replay never repeats the link");
    let own = pb::GetGrantRequest { id: grant.id.clone(), ..Default::default() };
    let own = h.grants(h.config(Some(&issued.link_token))).get_grant(own).await.unwrap();
    let own = own.into_owned();
    assert_eq!(own.grant.as_option().unwrap().actions, ["org.read"], "what it grants");
    assert!(own.chain.is_empty(), "and not the authority of whoever shared it");

    let listed = h
        .orgs(h.config(Some(&issued.link_token)))
        .list_organizations(pb::ListOrganizationsRequest::default())
        .await
        .unwrap()
        .into_owned();
    assert!(listed.organizations.is_empty(), "a listing spends nothing, so it shows nothing");
    read_org(&h, &issued.link_token).await.unwrap();
    read_org(&h, &issued.link_token).await.unwrap();
    let spent = read_org(&h, &issued.link_token).await.unwrap_err();
    assert_eq!(spent.code, ErrorCode::NotFound, "two uses, then none");

    let other = h.grants(h.root()).issue_grant(link(&h, "other", &["org.read"], 5)).await;
    let other = other.unwrap().into_owned();
    let revoke = pb::RevokeGrantRequest {
        mutation: mutation("revoke").into(),
        grant: other.grant.as_option().unwrap().id.clone(),
        ..Default::default()
    };
    h.grants(h.root()).revoke_grant(revoke).await.unwrap();
    let e = read_org(&h, &other.link_token).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unauthenticated, "revoked: the link works no more");
    let forged = format!("{}x", &other.link_token[..other.link_token.len() - 1]);
    assert_eq!(read_org(&h, &forged).await.unwrap_err().code, ErrorCode::Unauthenticated);
}

#[tokio::test]
async fn a_link_expires_limits_its_uses_names_one_object_and_delegates_nothing() {
    let h = Harness::new().await;
    let grants = h.grants(h.root());
    let field = async |r: pb::IssueGrantRequest| detail(&grants.issue_grant(r).await.unwrap_err());
    let mut r = link(&h, "a", &["org.read"], 3);
    r.conditions = pb::Conditions { max_uses: Some(3), ..Default::default() }.into();
    assert_eq!(field(r).await.field, "conditions.expires_at");
    let mut r = link(&h, "b", &["org.read"], 3);
    r.conditions =
        pb::Conditions { expires_at: Some(in_an_hour()).into(), ..Default::default() }.into();
    assert_eq!(field(r).await.field, "conditions.max_uses");
    let mut r = link(&h, "c", &["org.read"], 3);
    r.subject = principal::to_pb(&Principal::Link(uuid::Uuid::now_v7())).into();
    assert_eq!(field(r).await.field, "subject");
    let mut r = link(&h, "d", &["org.read"], 3);
    r.delegation =
        pb::DelegationPolicy { allowed: true, max_depth: Some(1), ..Default::default() }.into();
    assert_eq!(reason(&grants.issue_grant(r).await.unwrap_err()), "request.invalid");
    let r = link(&h, "e", &["workload.delete"], 3);
    assert_eq!(reason(&grants.issue_grant(r).await.unwrap_err()), "request.invalid");
    let mut issued_to_link = link(&h, "f", &["org.read"], 3);
    issued_to_link.link_form = false;
    issued_to_link.subject = principal::to_pb(&Principal::Link(uuid::Uuid::now_v7())).into();
    assert_eq!(field(issued_to_link).await.field, "subject", "a link's holder is made, not named");

    // A link makes no further link and issues nothing.
    let l = grants.issue_grant(link(&h, "g", &["org.read"], 3)).await.unwrap().into_owned();
    let holder = h.grants(h.config(Some(&l.link_token)));
    let again = holder.issue_grant(link(&h, "h", &["org.read"], 1)).await;
    assert_eq!(reason(&again.unwrap_err()), "request.invalid");
}

fn mint(h: &Harness, renews: &str, actions: &[&str]) -> pb::MintTokenRequest {
    pb::MintTokenRequest {
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope: org_scope(&h.root_org()).into(),
        renews: renews.to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_session_opened_through_a_link_spends_one_use_and_its_renewals_none() {
    let h = Harness::new().await;
    let made = h.grants(h.root()).issue_grant(link(&h, "s", &["org.read", "event.read"], 1)).await;
    let made = made.unwrap().into_owned();
    let as_link = h.grants(h.config(Some(&made.link_token)));
    let first = as_link.mint_token(mint(&h, "", &["org.read"])).await.unwrap().into_owned();
    let e = as_link.mint_token(mint(&h, "", &["org.read"])).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "its one use is the session");
    let mut token = first.token;
    for _ in 0..3 {
        let next = as_link.mint_token(mint(&h, &token, &["org.read"])).await.unwrap();
        token = next.into_owned().token;
    }
    let wider = as_link.mint_token(mint(&h, &token, &["org.read", "event.read"])).await;
    assert_eq!(detail(&wider.unwrap_err()).field, "renews", "a renewal adds no action");
    let theirs = h.grants(h.root()).mint_token(mint(&h, &token, &["org.read"])).await;
    assert_eq!(detail(&theirs.unwrap_err()).field, "renews", "nor changes hands");
    let revoke = pb::RevokeGrantRequest {
        mutation: mutation("end").into(),
        grant: made.grant.as_option().unwrap().id.clone(),
        ..Default::default()
    };
    h.grants(h.root()).revoke_grant(revoke).await.unwrap();
    let after = as_link.mint_token(mint(&h, &token, &["org.read"])).await.unwrap_err();
    assert_eq!(after.code, ErrorCode::Unauthenticated, "revoked: no renewal");
}

#[tokio::test]
async fn a_restart_proposed_and_confirmed_through_a_link_is_one_use() {
    use korpis_proto::korpis::v1::__buffa::oneof::scope::Kind as ScopeKind;
    let h = Harness::new().await;
    let (w, _) = crate::lifecycle::workload(&h).await;
    let mut request = link(&h, "share", &["workload.restart"], 1);
    request.scope =
        pb::Scope { kind: Some(ScopeKind::Workload(w.clone())), ..Default::default() }.into();
    let issued = h.grants(h.root()).issue_grant(request).await.unwrap().into_owned();
    let as_link = h.config(Some(&issued.link_token));
    let restart = |key: &str| pb::RestartRequest {
        mutation: crate::lifecycle::power(key, &w, 1).mutation,
        workload: w.clone(),
        propose: true,
        ..Default::default()
    };
    let proposed = h.workloads(as_link.clone()).restart(restart("r1")).await.unwrap().into_owned();
    let plan = proposed.plan.as_option().unwrap().id.clone();
    let confirm = crate::lifecycle::approve("a1", &plan);
    let confirmed = h.plans(as_link.clone()).approve_plan(confirm).await.unwrap().into_owned();
    let status = confirmed.plan.as_option().unwrap().status.as_known();
    assert_eq!(status, Some(pb::PlanStatus::PLAN_STATUS_APPROVED), "confirmed, spending nothing");
    let again = h.workloads(as_link).restart(restart("r2")).await.unwrap_err();
    assert_eq!(again.code, ErrorCode::NotFound, "and the one use is gone");
}
