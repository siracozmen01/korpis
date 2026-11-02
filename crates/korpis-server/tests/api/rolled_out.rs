// SPDX-License-Identifier: AGPL-3.0-or-later

//! A template rollout on a real control plane: two Moderators were issued from version 1, and
//! version 2 adds an action. The rollout is over both; approved under the approver's own grant,
//! it gives the one still held the addition as a grant of its own, attenuated from the approver's,
//! on the same project and naming version 2, and leaves the grant it adds to as it was. The other,
//! revoked after the approval, fails alone, and the rollout ends failed, saying how many.

use crate::drained::tended;
use crate::recipes::project;
use crate::support::{Harness, mutation};
use korpis_core::authz::{Kind, ObjectRef, Scope};
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::{principal, scope};

fn moderator(actions: &[&str]) -> pb::GrantTemplateBody {
    pb::GrantTemplateBody {
        name: "Moderator".into(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope_pattern: pb::ScopePattern {
            kind: pb::ObjectKind::OBJECT_KIND_PROJECT.into(),
            ..Default::default()
        }
        .into(),
        delegation: pb::DelegationPolicy::default().into(),
        ..Default::default()
    }
}

/// A grant from `template` at version 1 on `project`, to a new token.
async fn issued(h: &Harness, template: &str, project: &str) -> pb::Grant {
    let (token, _) = h.bare_token().await;
    let id = Id::parse(IdKind::Project, project).unwrap().uuid();
    let request = pb::IssueGrantRequest {
        mutation: mutation(&format!("issue-{token}")).into(),
        parent: h.root_grant(),
        subject: principal::to_pb(&Principal::Token(token)).into(),
        scope: scope::to_pb(&Scope::Object(ObjectRef::new(Kind::Project, id))).into(),
        template: pb::TemplateRef { template: template.into(), version: 1, ..Default::default() }
            .into(),
        ..Default::default()
    };
    let answer = h.grants(h.root()).issue_grant(request).await.unwrap().into_owned();
    answer.grant.into_option().unwrap()
}

async fn grant(h: &Harness, id: &str) -> pb::Grant {
    let request = pb::GetGrantRequest { id: id.to_owned(), ..Default::default() };
    let answer = h.grants(h.root()).get_grant(request).await.unwrap().into_owned();
    answer.grant.into_option().unwrap()
}

/// A Moderator template whose version 2 adds `workload.restart` to version 1's
/// `workload.read`, and two grants issued from version 1 on a project.
async fn moderators(h: &Harness) -> (String, pb::Grant, pb::Grant) {
    let project = project(h, &h.root_org(), "games").await;
    let grants = h.grants(h.root());
    let create = pb::CreateGrantTemplateRequest {
        mutation: mutation("moderator").into(),
        organization: h.root_org(),
        template: moderator(&["workload.read"]).into(),
        ..Default::default()
    };
    let made = grants.create_grant_template(create).await.unwrap().into_owned();
    let template = made.template.into_option().unwrap().id;
    let (kept, revoked) =
        (issued(h, &template, &project).await, issued(h, &template, &project).await);
    let edit = pb::UpdateGrantTemplateRequest {
        mutation: pb::Mutation { expected_version: Some(1), ..mutation("v2") }.into(),
        id: template.clone(),
        template: moderator(&["workload.read", "workload.restart"]).into(),
        ..Default::default()
    };
    grants.update_grant_template(edit).await.unwrap();
    (template, kept, revoked)
}

#[tokio::test]
async fn a_rollout_gives_each_holder_what_the_version_adds_and_a_revoked_one_fails_alone() {
    let h = Harness::new().await;
    let (template, kept, revoked) = moderators(&h).await;
    let grants = h.grants(h.root());
    let policy = pb::OperationPolicy {
        on_failure: pb::OnFailure::ON_FAILURE_CONTINUE.into(),
        ..Default::default()
    };
    let request = pb::ProposeOperationRequest {
        mutation: mutation("rollout").into(),
        kind: pb::OperationKind::OPERATION_KIND_TEMPLATE_ROLLOUT.into(),
        template: template.clone(),
        template_version: 2,
        policy: policy.into(),
        ..Default::default()
    };
    let operations = h.operations(h.root());
    let op = operations.propose_operation(request).await.unwrap().into_owned();
    let op = op.operation.into_option().unwrap();
    assert_eq!(op.items.len(), 2, "both Moderators of version 1");
    crate::drained::approved(&h, &op.id, &h.root_grant()).await;
    let revoke = pb::RevokeGrantRequest {
        mutation: mutation("revoke").into(),
        grant: revoked.id.clone(),
        ..Default::default()
    };
    grants.revoke_grant(revoke).await.unwrap();

    tended(&h, &op.id).await;
    let ended = tended(&h, &op.id).await;
    assert_eq!(ended.status.as_known(), Some(pb::OperationStatus::OPERATION_STATUS_FAILED));
    assert_eq!(ended.reason, "1 of 2 items failed");
    let item = |g: &str| ended.items.iter().find(|i| i.subject == g).unwrap().clone();
    let failed = item(&revoked.id);
    assert_eq!(failed.state.as_known(), Some(pb::OperationItemState::OPERATION_ITEM_STATE_FAILED));
    let given = item(&kept.id);
    assert_eq!(given.state.as_known(), Some(pb::OperationItemState::OPERATION_ITEM_STATE_DONE));
    let addition = grant(&h, given.issued.as_deref().unwrap()).await;
    assert_eq!(addition.actions, ["workload.restart"], "what version 2 adds, and only that");
    assert_eq!(addition.parent.as_deref(), Some(h.root_grant().as_str()), "the approver's");
    assert_eq!(addition.scope, kept.scope, "on the same project");
    assert_eq!(addition.subject, kept.subject, "to the same holder");
    assert_eq!(addition.from_template.as_option().map(|t| t.version), Some(2));
    assert_eq!(grant(&h, &kept.id).await.actions, ["workload.read"], "untouched");
}
