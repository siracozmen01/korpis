// SPDX-License-Identifier: AGPL-3.0-or-later

//! Domains (§3.2 of docs/07-networking.md): asked for under `domain.write`, verified by a TXT
//! record the control plane resolves, by delegation beneath an ancestor's verified domain, or by
//! an operator's attestation; refused where another organization holds a verified domain
//! overlapping the name; deleted with what was delegated beneath them.

use crate::support::{Harness, mutation, org_scope, reason};
use crate::zone::Zone;
use connectrpc::ErrorCode;
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::principal;

use pb::DomainVerification as V;

fn ask(org: &str, key: &str, name: &str, how: V, parent: Option<&str>) -> pb::CreateDomainRequest {
    pb::CreateDomainRequest {
        mutation: mutation(key).into(),
        organization: org.into(),
        name: name.into(),
        verification: how.into(),
        parent: parent.map(str::to_owned),
        ..Default::default()
    }
}

fn verify(key: &str, id: &str) -> pb::VerifyDomainRequest {
    pb::VerifyDomainRequest { mutation: mutation(key).into(), id: id.into(), ..Default::default() }
}

/// A child organization of the root.
pub async fn child(h: &Harness, name: &str) -> String {
    let made = h
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
    made.organization.as_option().unwrap().id.clone()
}

async fn create(h: &Harness, req: pb::CreateDomainRequest) -> pb::Domain {
    let made = h.domains(h.root()).create_domain(req).await.unwrap().into_owned();
    made.domain.into_option().unwrap()
}

#[tokio::test]
async fn a_domain_is_verified_by_its_txt_record_and_then_held_against_other_organizations() {
    let (zone, at) = Zone::serve().await;
    let h = Harness::with_resolver(at).await;
    let domains = h.domains(h.root());
    let d =
        create(&h, ask(&h.root_org(), "d1", "example.com", V::DOMAIN_VERIFICATION_DNS_TXT, None))
            .await;
    assert!(d.id.starts_with("dom_"));
    assert_eq!(d.record, "_korpis-verify.example.com");
    assert_eq!(d.token.len(), 48);
    assert!(d.verified_at.as_option().is_none(), "pending");
    let e = domains.verify_domain(verify("v1", &d.id)).await.unwrap_err();
    assert_eq!(reason(&e), "domain.unverified", "nothing is published yet");

    zone.publish(&d.record, "unrelated");
    zone.publish(&d.record, &d.token);
    let v = domains.verify_domain(verify("v2", &d.id)).await.unwrap().into_owned();
    assert!(v.domain.as_option().unwrap().verified_at.as_option().is_some());
    zone.withdraw(&d.record);
    let again = domains.verify_domain(verify("v3", &d.id)).await.unwrap().into_owned();
    assert!(again.domain.as_option().unwrap().verified_at.as_option().is_some(), "kept");

    let beta = child(&h, "beta").await;
    for (n, name) in ["example.com", "shop.example.com"].into_iter().enumerate() {
        let theirs =
            create(&h, ask(&beta, &format!("b{n}"), name, V::DOMAIN_VERIFICATION_DNS_TXT, None))
                .await;
        zone.publish(&theirs.record, &theirs.token);
        let e = domains.verify_domain(verify(&format!("bv{n}"), &theirs.id)).await.unwrap_err();
        assert_eq!(reason(&e), "domain.held", "{name} overlaps example.com");
    }
    let unrelated =
        create(&h, ask(&beta, "b3", "badexample.com", V::DOMAIN_VERIFICATION_DNS_TXT, None)).await;
    zone.publish(&unrelated.record, &unrelated.token);
    domains.verify_domain(verify("bv3", &unrelated.id)).await.expect("a suffix that is no label");
    let e = domains
        .create_domain(ask(
            &h.root_org(),
            "d2",
            "example.com",
            V::DOMAIN_VERIFICATION_DNS_TXT,
            None,
        ))
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "domain.taken", "an organization asks for a name once");
}

/// An attested `example.com` of the root organization, and `shop.example.com` delegated beneath
/// it to the child organization `beta`.
async fn delegated(h: &Harness) -> (pb::Domain, String, pb::Domain) {
    let apex =
        create(h, ask(&h.root_org(), "d1", "example.com", V::DOMAIN_VERIFICATION_OPERATOR, None))
            .await;
    assert!(apex.verified_at.as_option().is_some(), "attested is verified at once");
    assert!(apex.token.is_empty() && apex.record.is_empty());
    let beta = child(h, "beta").await;
    let shop = create(
        h,
        ask(&beta, "d2", "shop.example.com", V::DOMAIN_VERIFICATION_DELEGATED, Some(&apex.id)),
    )
    .await;
    assert!(shop.verified_at.as_option().is_some(), "delegated is verified at once");
    assert_eq!(shop.parent.as_deref(), Some(apex.id.as_str()));
    (apex, beta, shop)
}

#[tokio::test]
async fn a_delegation_is_from_above_to_beneath_and_attestation_overrides_no_holder() {
    let h = Harness::new().await;
    let domains = h.domains(h.root());
    let (apex, beta, shop) = delegated(&h).await;
    let gamma = child(&h, "gamma").await;
    let e = domains
        .create_domain(ask(
            &gamma,
            "d3",
            "a.shop.example.com",
            V::DOMAIN_VERIFICATION_DELEGATED,
            Some(&shop.id),
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument, "beta is not above gamma");
    let e = domains
        .create_domain(ask(
            &beta,
            "d4",
            "other.org",
            V::DOMAIN_VERIFICATION_DELEGATED,
            Some(&apex.id),
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument, "not beneath the parent");
    let e = domains
        .create_domain(ask(&gamma, "d5", "example.com", V::DOMAIN_VERIFICATION_OPERATOR, None))
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "domain.held", "attestation does not override a holder");
}

#[tokio::test]
async fn a_name_beneath_an_ancestors_domain_is_delegated_and_goes_with_it() {
    let h = Harness::new().await;
    let domains = h.domains(h.root());
    let (apex, beta, shop) = delegated(&h).await;
    let listed = domains
        .list_domains(pb::ListDomainsRequest { organization: beta.clone(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(
        listed.domains.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["shop.example.com"]
    );
    let del = pb::DeleteDomainRequest {
        mutation: mutation("x1").into(),
        id: apex.id.clone(),
        ..Default::default()
    };
    let gone = domains.delete_domain(del).await.unwrap().into_owned();
    assert_eq!(gone.deleted, 2, "the delegation goes with its parent");
    let e = domains
        .get_domain(pb::GetDomainRequest { id: shop.id.clone(), ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn delegating_and_attesting_need_authority_above_the_holder() {
    let h = Harness::new().await;
    let apex =
        create(&h, ask(&h.root_org(), "d1", "example.com", V::DOMAIN_VERIFICATION_OPERATOR, None))
            .await;
    let beta = child(&h, "beta").await;
    let (token, secret) = h.bare_token().await;
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("g1").into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec!["domain.write".into(), "domain.read".into()],
            scope: org_scope(&beta).into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let theirs = h.domains(h.config(Some(secret.expose())));
    theirs
        .create_domain(ask(&beta, "t1", "beta.example.org", V::DOMAIN_VERIFICATION_DNS_TXT, None))
        .await
        .expect("domain.write on its own organization");
    let e = theirs
        .create_domain(ask(
            &beta,
            "t2",
            "shop.example.com",
            V::DOMAIN_VERIFICATION_DELEGATED,
            Some(&apex.id),
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "the parent's holder is not its to act for: {e:?}");
    let e = theirs
        .create_domain(ask(&beta, "t3", "beta.example.net", V::DOMAIN_VERIFICATION_OPERATOR, None))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "attesting is the root's: {e:?}");
    let e = theirs
        .get_domain(pb::GetDomainRequest { id: apex.id.clone(), ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "another organization's domain is not seen");
}

#[tokio::test]
async fn an_operator_attests_for_any_organization_beneath_with_domain_attest_alone() {
    let h = Harness::new().await;
    let beta = child(&h, "beta").await;
    let (token, secret) = h.bare_token().await;
    h.issue_to(token, "g1", &["domain.attest"]).await;
    let operator = h.domains(h.config(Some(secret.expose())));
    let made = operator
        .create_domain(ask(&beta, "a1", "beta.internal", V::DOMAIN_VERIFICATION_OPERATOR, None))
        .await
        .expect("attested for a descendant")
        .into_owned();
    let d = made.domain.into_option().unwrap();
    assert!(d.verified_at.as_option().is_some());
    assert_eq!(d.organization, beta);
    let e = operator
        .create_domain(ask(&beta, "a2", "beta.example.org", V::DOMAIN_VERIFICATION_DNS_TXT, None))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "asking for one to verify is domain.write: {e:?}");
}
