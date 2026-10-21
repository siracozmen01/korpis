// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a person signed in through their identity provider passes on through their set (§5.1 of
//! docs/08-identity.md), issued from their own session: it names the set and the person it came
//! through, its expiry is clamped to the delegation ceiling, it works while the set still has
//! them, and it is refused once the set's re-verification finds they left, without anyone
//! revoking it.

use crate::bindings::{bind, from_template, project_scope, set};
use crate::delegated::readers;
use crate::external_complete::signed_in;
use crate::external_sign_in::{GONE, issuing};
use crate::recipes::project;
use crate::support::{Harness, mutation};
use buffa_types::google::protobuf::Timestamp;
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seven days, the delegation ceiling where no organization sets one.
const CEILING: Duration = Duration::from_secs(7 * 24 * 3600);

fn at(t: SystemTime) -> Timestamp {
    let since = t.duration_since(UNIX_EPOCH).unwrap();
    Timestamp { seconds: i64::try_from(since.as_secs()).unwrap(), ..Default::default() }
}

/// The ops set bound with the Readers template and given it on `project`, verified: the set's
/// grant and the binding.
async fn ops_read(h: &Harness, project: &str) -> (String, String) {
    let org = h.root_org();
    let grants = h.grants(h.root());
    let create = pb::CreateGrantTemplateRequest {
        mutation: mutation("readers").into(),
        organization: org.clone(),
        template: readers().into(),
        ..Default::default()
    };
    let tpl = grants.create_grant_template(create).await.unwrap().into_owned();
    let tpl = tpl.template.into_option().unwrap().id;
    let terms = pb::IdentityBindingTerms {
        template: Some(pb::TemplateRef { template: tpl.clone(), version: 1, ..Default::default() })
            .into(),
        ..Default::default()
    };
    let binding = bind(h, &org, set(&org, "ops"), terms).await.unwrap();
    verify(h, &binding.id).await;
    let issued = grants.issue_grant(from_template(h, &set(&org, "ops"), (&tpl, 1), project)).await;
    (issued.unwrap().into_owned().grant.into_option().unwrap().id, binding.id)
}

async fn verify(h: &Harness, binding: &str) {
    let req = pb::VerifyIdentityBindingRequest { id: binding.to_owned(), ..Default::default() };
    h.identity(h.root()).verify_identity_binding(req).await.unwrap();
}

/// Passing on `parent` on `project` to a new token for thirty days, as the mutation `key`.
fn helper(key: &str, parent: &str, project: &str) -> pb::IssueGrantRequest {
    pb::IssueGrantRequest {
        mutation: mutation(key).into(),
        parent: parent.to_owned(),
        actions: vec!["project.read".into()],
        scope: project_scope(project).into(),
        conditions: pb::Conditions {
            expires_at: Some(at(SystemTime::now() + Duration::from_secs(30 * 24 * 3600))).into(),
            ..Default::default()
        }
        .into(),
        to_new_token: true,
        label: "a helper".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_person_passes_on_through_their_set_until_they_leave_it() {
    let h = Harness::new().await;
    let _corp = issuing(&h, 75, "corp", true).await;
    let survival = project(&h, &h.root_org(), "survival").await;
    let (role, binding) = ops_read(&h, &survival).await;
    let (_, token) = signed_in(&h, "erin", "ops", "p1").await;

    let issued =
        h.grants(h.config(Some(&token))).issue_grant(helper("helper", &role, &survival)).await;
    let issued = issued.expect("she holds the set's grant while the set has her").into_owned();
    let grant = issued.grant.into_option().unwrap();
    let via = grant.via.into_option().expect("it names the set and the member it came through");
    let named = |i: &pb::ExternalIdentity| format!("{:?}", i.selector);
    assert!(named(via.set.as_option().unwrap()).contains("ops"), "{via:?}");
    assert!(named(via.member.as_option().unwrap()).contains("erin"), "{via:?}");
    let expires = grant.conditions.as_option().unwrap().expires_at.as_option().unwrap().seconds;
    let ceiling = at(SystemTime::now() + CEILING).seconds;
    assert!(expires <= ceiling, "thirty days asked, the ceiling's seven shown: {expires}");

    let reader = h.projects(h.config(Some(&issued.token)));
    let get = || pb::GetProjectRequest { id: survival.clone(), ..Default::default() };
    reader.get_project(get()).await.expect("what she passed on works while ops has her");

    GONE.lock().unwrap().push("erin".into());
    verify(&h, &binding).await;
    let left = reader.get_project(get()).await.unwrap_err();
    assert_eq!(left.code, ErrorCode::NotFound, "she left ops, and nobody revoked anything");
    let again =
        h.grants(h.config(Some(&token))).issue_grant(helper("another", &role, &survival)).await;
    let again = again.unwrap_err();
    assert_eq!(again.code, ErrorCode::InvalidArgument, "she no longer holds it: {again:?}");
}
