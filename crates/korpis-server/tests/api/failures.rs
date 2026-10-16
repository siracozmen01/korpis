// SPDX-License-Identifier: AGPL-3.0-or-later

//! Failed attempts to authenticate (§10 of docs/10-api.md): each is charged to its source and
//! answered `RATE_LIMITED` once the source's bucket refuses, while a credential that opens is
//! admitted from the same source all the same; and each is recorded as a denial is, by no one,
//! the first at once and its repeats within the window as one effect with their count.

use crate::agent::{TestAgent, agents};
use crate::support::{Harness, reason};
use connectrpc::ErrorCode;
use korpis_core::rate::{Rate, Rates};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

/// Every effect recording a failure of `action`: its actor, count, node, and source.
async fn failures(h: &Harness, action: &str) -> Vec<(String, i64, Option<uuid::Uuid>, String)> {
    let store = h.db.store().await.unwrap();
    sqlx::query_as(
        "SELECT actor, count, node_id, error->>'source' FROM effect
          WHERE action = $1 AND outcome = 'denied' ORDER BY at, id",
    )
    .bind(action)
    .fetch_all(store.pool())
    .await
    .unwrap()
}

/// The failures of `action` once `done` holds of them, within ten seconds.
async fn recorded(
    h: &Harness,
    action: &str,
    done: impl Fn(&[(String, i64, Option<uuid::Uuid>, String)]) -> bool,
) -> Vec<(String, i64, Option<uuid::Uuid>, String)> {
    for _ in 0..100 {
        let found = failures(h, action).await;
        if done(&found) {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    failures(h, action).await
}

/// Three failures a minute from each source, at most three at once; every other rate unmet.
fn three_a_minute() -> Rates {
    let ample = Rate::per_second(1000, 1000);
    Rates {
        individual: ample,
        organization: ample,
        expensive_individual: ample,
        expensive_organization: ample,
        failures: Rate::per_minute(3, 3),
    }
}

#[tokio::test]
async fn a_source_failing_is_told_to_wait_but_a_working_credential_from_it_is_admitted() {
    let h = Harness::with_rates(three_a_minute()).await;
    let forged = h.config(Some("kpt_not-a-token-this-installation-issued"));
    let get = || pb::GetOrganizationRequest { id: h.root_org(), ..Default::default() };

    let mut codes = Vec::new();
    for _ in 0..4 {
        codes.push(h.orgs(forged.clone()).get_organization(get()).await.unwrap_err());
    }
    let first: Vec<_> = codes[..3].iter().map(|e| e.code).collect();
    assert_eq!(first, vec![ErrorCode::Unauthenticated; 3]);
    assert_eq!(codes[3].code, ErrorCode::ResourceExhausted);
    assert_eq!(reason(&codes[3]), "rate.limited");
    assert!(codes[3].response_headers().contains_key("retry-after"));
    h.orgs(h.root()).get_organization(get()).await.expect("a working token is admitted");

    let at_once = recorded(&h, "credential.present", |f| f.len() == 2).await;
    let summary: Vec<_> = at_once.iter().map(|(a, n, _, s)| (a.as_str(), *n, s.as_str())).collect();
    assert_eq!(
        summary,
        vec![("anonymous", 1, "127.0.0.1/32"), ("anonymous", 3, "127.0.0.1/32")],
        "the first at once, its three repeats as one once the window closed"
    );
}

#[tokio::test]
async fn a_forged_hello_is_recorded_against_the_node_it_named() {
    let h = Harness::with_rates(three_a_minute()).await;
    let a = TestAgent::enroll(&h, "fra-1", 1).await;
    let forger = TestAgent { key: ed25519_dalek::SigningKey::from_bytes(&[9; 32]), ..a };
    let (_s, refused) = TestAgent::open(&h, forger.hello_at(SystemTime::now(), &[7; 16])).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::Unauthenticated);

    let node = korpis_core::id::Id::parse(korpis_core::id::IdKind::Node, &forger.node).unwrap();
    let found = recorded(&h, "node.hello", |f| !f.is_empty()).await;
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].0.as_str(), found[0].2), ("anonymous", Some(node.uuid())));

    let real = TestAgent { key: ed25519_dalek::SigningKey::from_bytes(&[1; 32]), ..forger };
    let (_s, opened) = TestAgent::open(&h, real.hello_at(SystemTime::now(), &[8; 16])).await;
    opened.expect("the node's own Hello still opens its session");
}

#[tokio::test]
async fn joining_with_a_token_that_does_not_exist_is_a_failure() {
    let h = Harness::with_rates(three_a_minute()).await;
    let join = agent::EnrollRequest {
        enrollment_token: "kpe_not-a-token".into(),
        node_public_key: vec![1; 32],
        ..Default::default()
    };
    let e = agents(&h).enroll(join).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unauthenticated);
    let found = recorded(&h, "node.join", |f| !f.is_empty()).await;
    assert_eq!(found.len(), 1, "{found:?}");
}
