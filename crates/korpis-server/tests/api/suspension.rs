// SPDX-License-Identifier: AGPL-3.0-or-later

//! Suspending an organization (§2.1 of docs/08-identity.md), on a real control plane: the host
//! suspends a reseller's sub-tenant, whose grants then make no change and read only its grants,
//! its tree, and the status of what runs in it; the sub-tenant cannot resume itself; a second
//! suspension is a conflict; a root is suspended by no one; and resuming restores every grant as
//! it was, the two recorded as the host's acts.

use crate::delegation::{Holder, organization, reseller_and_sub_tenant};
use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

fn suspend(org: &str, key: &str) -> pb::SuspendOrganizationRequest {
    pb::SuspendOrganizationRequest {
        mutation: mutation(key).into(),
        organization: org.to_owned(),
        reason: "the invoice of October is unpaid".into(),
        on_suspend: pb::OnSuspend::ON_SUSPEND_KEEP_RUNNING.into(),
        ..Default::default()
    }
}

fn resume(org: &str, key: &str) -> pb::ResumeOrganizationRequest {
    pb::ResumeOrganizationRequest {
        mutation: mutation(key).into(),
        organization: org.to_owned(),
        ..Default::default()
    }
}

/// Whether `by` may make a project named `name` in organization `org`.
async fn makes_project(harness: &Harness, by: &Holder, org: &str, name: &str) -> bool {
    let request = pb::CreateProjectRequest {
        mutation: mutation(&format!("project-{name}")).into(),
        organization: org.to_owned(),
        name: name.into(),
        ..Default::default()
    };
    harness.projects(by.config.clone()).create_project(request).await.is_ok()
}

#[tokio::test]
async fn a_suspended_sub_tenant_reads_what_it_owes_for_and_changes_nothing_until_resumed() {
    let harness = Harness::new().await;
    let (_reseller, sub_tenant, sub) = reseller_and_sub_tenant(&harness).await;
    let customer = organization(&harness, &sub_tenant, &sub, "customer").await;
    assert!(makes_project(&harness, &sub_tenant, &customer, "before").await);

    let host = harness.orgs(harness.root());
    let done = host.suspend_organization(suspend(&sub, "s1")).await.unwrap().into_owned();
    let shown = done.organization.into_option().unwrap();
    assert_eq!(
        shown.status.as_known(),
        Some(pb::OrganizationStatus::ORGANIZATION_STATUS_SUSPENDED)
    );
    let suspension = shown.suspension.into_option().unwrap();
    assert_eq!(suspension.reason, "the invoice of October is unpaid");

    // Its grants, and the customer's beneath them, make nothing; its tree stays readable.
    assert!(!makes_project(&harness, &sub_tenant, &customer, "during").await, "no change");
    let own = pb::GetOrganizationRequest { id: sub.clone(), ..Default::default() };
    let read = harness.orgs(sub_tenant.config.clone()).get_organization(own).await.unwrap();
    let read = read.into_owned().organization.into_option().unwrap();
    assert_eq!(read.suspension.into_option().unwrap().reason, suspension.reason, "and says why");

    // It cannot resume itself, and suspending it again is a conflict.
    let tenant = harness.orgs(sub_tenant.config.clone());
    let refusal = tenant.resume_organization(resume(&sub, "r0")).await.unwrap_err();
    assert_eq!(refusal.code, ErrorCode::NotFound, "nothing above it is there for it");
    let again = host.suspend_organization(suspend(&sub, "s2")).await.unwrap_err();
    assert!(again.message.as_deref().unwrap_or_default().contains("already"), "{again:?}");
    let root = host.suspend_organization(suspend(&harness.root_org(), "s3")).await.unwrap_err();
    assert_eq!(root.code, ErrorCode::InvalidArgument, "a root has nothing above it");

    host.resume_organization(resume(&sub, "r1")).await.unwrap();
    assert!(makes_project(&harness, &sub_tenant, &customer, "after").await, "as it was");
    let succeeded = |rows: Vec<(String, Option<uuid::Uuid>)>| {
        rows.into_iter()
            .filter(|(outcome, grant)| outcome == "succeeded" && grant.is_some())
            .count()
    };
    assert_eq!(succeeded(harness.effects("org.suspend").await), 1, "one suspension, the host's");
    assert_eq!(succeeded(harness.effects("org.resume").await), 1, "one resumption, the host's");
}
