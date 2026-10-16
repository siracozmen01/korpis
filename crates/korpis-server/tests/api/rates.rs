// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rate limits on the client API (§10 of docs/10-api.md): each request is charged to its subject's
//! bucket and to the bucket of each organization the subject holds a grant in, so a grant minted
//! by attenuation adds no capacity; the requests that compute a plan, back up, or install are also
//! charged to a budget of their own; every response says where the caller stands; and every
//! replica charges the same buckets.

use crate::support::{Harness, detail, mutation, org_scope, reason};
use connectrpc::client::ClientConfig;
use connectrpc::{ConnectError, ErrorCode};
use korpis_core::principal::Principal;
use korpis_core::rate::{Rate, Rates};
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::principal;
use std::time::{Duration, Instant};

/// Rates no test here reaches, but for those it sets.
fn ample() -> Rates {
    let rate = Rate::per_second(1000, 1000);
    Rates {
        individual: rate,
        organization: rate,
        expensive_individual: rate,
        expensive_organization: rate,
        failures: rate,
        recorded: rate,
        guesses: rate,
    }
}

/// A token holding `org.read` and `workload.read` on the root organization, issued from `parent`
/// by whoever `issuer` presents, and allowed to issue them on if `delegates`; its secret and its
/// grant.
async fn reader(
    h: &Harness,
    (issuer, parent): (ClientConfig, String),
    key: &str,
    delegates: bool,
) -> (String, String) {
    let (token, secret) = h.bare_token().await;
    let granted = h
        .grants(issuer)
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation(key).into(),
            parent,
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec!["org.read".into(), "workload.read".into()],
            scope: org_scope(&h.root_org()).into(),
            delegation: pb::DelegationPolicy {
                allowed: delegates,
                max_depth: Some(u32::from(delegates)),
                actions: vec!["org.read".into(), "workload.read".into()],
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    (secret.expose().to_owned(), granted.grant.as_option().unwrap().id.clone())
}

/// Reads the root organization as `config` presents.
async fn read(h: &Harness, config: ClientConfig) -> Result<(), ConnectError> {
    let get = pb::GetOrganizationRequest { id: h.root_org(), ..Default::default() };
    h.orgs(config).get_organization(get).await.map(|_| ())
}

/// Reads as `config` presents until refused, within `most` requests; the refusal, when it came,
/// and how many were admitted.
async fn exhaust(h: &Harness, config: &ClientConfig, most: u32) -> (ConnectError, Instant, u32) {
    for admitted in 0..=most {
        if let Err(e) = read(h, config.clone()).await {
            return (e, Instant::now(), admitted);
        }
    }
    panic!("{most} requests were all admitted");
}

/// When a refusal says the request would be admitted.
fn retry_after(e: &ConnectError) -> Duration {
    let d = detail(e).retry_after.into_option().unwrap_or_default();
    Duration::new(u64::try_from(d.seconds).unwrap(), u32::try_from(d.nanos).unwrap())
}

#[tokio::test]
async fn a_subject_is_refused_past_its_own_bucket_and_told_where_it_stands() {
    let h = Harness::with_rates(Rates { individual: Rate::per_second(1, 5), ..ample() }).await;
    let (secret, _) = reader(&h, (h.root(), h.root_grant()), "g1", false).await;
    let config = h.config(Some(&secret));

    let get = pb::GetOrganizationRequest { id: h.root_org(), ..Default::default() };
    let answered = h.orgs(config.clone()).get_organization(get).await.unwrap();
    let header = |name: &str| answered.headers().get(name).unwrap().to_str().unwrap().to_owned();
    let policy = header("ratelimit-policy");
    assert!(policy.contains("\"individual\";q=5;w=5"), "{policy}");
    assert!(policy.contains("\"organization\";q=1000;w=1"), "{policy}");
    assert!(header("ratelimit").contains("\"individual\";r="), "{}", header("ratelimit"));

    let (refused, _, admitted) = exhaust(&h, &config, 10).await;
    assert!(admitted <= 5, "{admitted} admitted past a burst of five");
    assert_eq!(refused.code, ErrorCode::ResourceExhausted);
    assert_eq!(reason(&refused), "rate.limited");
    assert!(detail(&refused).retryable);
    assert!(retry_after(&refused) <= Duration::from_secs(1));
    assert!(refused.response_headers().contains_key("retry-after"));
    read(&h, h.root()).await.expect("another subject has its own bucket");
}

#[tokio::test]
async fn a_grant_minted_by_attenuation_shares_its_organizations_bucket() {
    let organization = Rate::per_second(1, 8);
    let h = Harness::with_rates(Rates { organization, ..ample() }).await;
    let (a, grant) = reader(&h, (h.root(), h.root_grant()), "g1", true).await;
    let (b, _) = reader(&h, (h.config(Some(&a)), grant), "g2", false).await;

    let (refused, at, _) = exhaust(&h, &h.config(Some(&b)), 10).await;
    assert_eq!(reason(&refused), "rate.limited", "{refused:?}");
    let other = read(&h, h.config(Some(&a))).await;
    if at.elapsed() < retry_after(&refused) {
        let e = other.expect_err("the issuer is refused by the organization's bucket too");
        assert_eq!(e.code, ErrorCode::ResourceExhausted);
    }
}

#[tokio::test]
async fn the_expensive_budget_refuses_alone_and_leaves_reads_admitted() {
    let expensive_individual = Rate::per_second(1, 2);
    let h = Harness::with_rates(Rates { expensive_individual, ..ample() }).await;
    let (secret, _) = reader(&h, (h.root(), h.root_grant()), "g1", false).await;
    let config = h.config(Some(&secret));
    let start = || pb::StartRequest {
        mutation: mutation("s").into(),
        workload: "wl_unknown".into(),
        ..Default::default()
    };

    let mut refused = None;
    for _ in 0..4 {
        match h.workloads(config.clone()).start(start()).await {
            Err(e) if e.code == ErrorCode::ResourceExhausted => {
                refused = Some(e);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(reason(&refused.expect("refused within four")), "rate.limited");
    read(&h, config).await.expect("reads have their own budget");
}

#[tokio::test]
async fn every_replica_charges_the_same_buckets() {
    let h = Harness::with_rates(Rates { individual: Rate::per_second(1, 4), ..ample() }).await;
    let (secret, _) = reader(&h, (h.root(), h.root_grant()), "g1", false).await;
    let replica =
        korpis_server::ServeConfig { listen: "127.0.0.1:0".parse().unwrap(), ..h.served.clone() };
    let replica = korpis_server::serve(replica).await.expect("a second replica");
    let base = format!("https://localhost:{}", replica.addr.port()).parse().unwrap();
    let there =
        ClientConfig::new(base).with_default_header("authorization", format!("Bearer {secret}"));
    let here = h.config(Some(&secret));

    let began = Instant::now();
    let mut admitted = 0;
    for i in 0..20 {
        let config = if i % 2 == 0 { here.clone() } else { there.clone() };
        if read(&h, config).await.is_err() {
            break;
        }
        admitted += 1;
    }
    let refilled = began.elapsed().as_secs() + 1;
    assert!(admitted <= 4 + refilled, "{admitted} admitted across two replicas");
    replica.task.abort();
}
