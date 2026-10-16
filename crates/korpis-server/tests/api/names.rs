// SPDX-License-Identifier: AGPL-3.0-or-later

//! The names of ingress endpoints in DNS (§3.5 of docs/07-networking.md): each name an endpoint
//! routes is published through the organization's DNS provider that serves its zone, resolving
//! to its node's advertised addresses, and published again when they change; a name no provider
//! serves is its holder's, and its status says so; a name no longer routed is withdrawn.

use crate::agent::{Stream, TestAgent, welcome};
use crate::extensions::{DNS, push_document};
use crate::journal::{assignment, complete, until};
use crate::providers::{Installed, installed_from};
use crate::recipes::{change, project, push_paper};
use crate::routes::{attest, declare, web};
use crate::support::Harness;
use korpis_extension::dns::{Dns, DnsProvider, Failure};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

/// Every owner's records, for names under `example.com`.
#[derive(Clone, Default)]
struct Zone(Arc<Mutex<BTreeMap<String, Vec<pb::DnsRecord>>>>);

impl Zone {
    fn values(&self) -> Vec<(String, Vec<String>)> {
        let held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut all: Vec<_> =
            held.values().flatten().map(|r| (r.name.clone(), r.values.clone())).collect();
        all.sort();
        all
    }
}

impl DnsProvider for Zone {
    fn apply(
        &self,
        owner: String,
        records: Vec<pb::DnsRecord>,
    ) -> impl Future<Output = Result<pb::RecordsApplied, Failure>> + Send {
        let refuse = Failure {
            reason: "dns.zone_not_found".into(),
            message: String::new(),
            retryable: false,
        };
        let answer = if records.iter().any(|r| !r.name.ends_with(".example.com")) {
            Err(refuse)
        } else {
            let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            if records.is_empty() {
                held.remove(&owner);
            } else {
                held.insert(owner, records);
            }
            Ok(pb::RecordsApplied::default())
        };
        std::future::ready(answer)
    }
}

/// The DNS provider `cloudflare`, serving `zone`; its installation, held while it serves.
async fn serving(h: &Harness, zone: Zone) -> Installed {
    let x = installed_from(h, 51, "dns", push_document(DNS).await).await;
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_DNS,
        name: "cloudflare".into(),
        version: "1.0.0".into(),
        concurrency: 4,
    };
    let provider = Arc::new(Provider::new(connection, registration, Dns(zone)));
    let mut registered = provider.registered();
    let run = Arc::clone(&provider);
    tokio::spawn(async move { run.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
    x
}

/// The names of `workload` as its status shows them, once `done` holds of them.
async fn shown(
    h: &Harness,
    workload: &str,
    done: impl Fn(&[pb::EndpointName]) -> bool,
) -> Vec<pb::EndpointName> {
    for _ in 0..300 {
        let get = pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() };
        let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
        let names = got.status.into_option().unwrap_or_default().names;
        if done(&names) {
            return names;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{workload}'s names did not settle");
}

fn state(n: &pb::EndpointName) -> Option<pb::EndpointNameState> {
    n.state.as_known()
}

/// What the zone holds, once it holds `expected`, within ten seconds.
async fn holds(zone: &Zone, expected: &[(&str, &str)]) {
    let wanted: Vec<(String, Vec<String>)> =
        expected.iter().map(|(n, v)| ((*n).to_owned(), vec![(*v).to_owned()])).collect();
    for _ in 0..100 {
        if zone.values() == wanted {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(zone.values(), wanted);
}

/// A node serving the ingress at `203.0.113.7` and `2001:db8::7`, in session, and a workload
/// routing `shop.example.com` and `shop.example.org`, its first plan completed; the journal
/// sequence used.
async fn shop(h: &Harness) -> (Stream, pb::Workload, agent::Assignment, u64) {
    let edge = TestAgent::enroll(h, "fra-1", 1).await;
    let (mut s, first) = TestAgent::open(h, edge.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    readdress(h, "203.0.113.7,2001:db8::7").await;
    let project = project(h, &h.root_org(), "shops").await;
    attest(h, "d1", &h.root_org(), "example.com").await;
    attest(h, "d2", &h.root_org(), "example.org").await;
    let names = ["shop.example.com", "shop.example.org"];
    let shop = declare(h, &project, "shop", web(&push_paper().await, &names)).await;
    let created = until(&mut s, assignment(|a| a.plan.as_option().is_some())).await;
    let seq = complete(&mut s, &shop.id, created.plan.as_option().unwrap(), 1).await;
    (s, shop, created, seq)
}

/// Makes every node serve the ingress at `addresses`, comma-separated.
async fn readdress(h: &Harness, addresses: &str) {
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE node SET ingress = true, addresses = string_to_array($1, ',')")
        .bind(addresses)
        .execute(store.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn an_endpoints_names_are_published_where_a_provider_serves_them_and_withdrawn() {
    use pb::EndpointNameState as S;
    let h = Harness::new().await;
    let zone = Zone::default();
    let _provider = serving(&h, zone.clone()).await;
    let (mut s, shop, created, seq) = shop(&h).await;

    let settled = |n: &[pb::EndpointName]| {
        n.len() == 2 && n.iter().all(|n| state(n) != Some(S::ENDPOINT_NAME_STATE_PENDING))
    };
    let both = shown(&h, &shop.id, settled).await;
    let first = (both[0].name.as_str(), state(&both[0]), both[0].provider.as_str());
    assert_eq!(first, ("shop.example.com", Some(S::ENDPOINT_NAME_STATE_PUBLISHED), "cloudflare"));
    let second = (both[1].name.as_str(), state(&both[1]));
    assert_eq!(second, ("shop.example.org", Some(S::ENDPOINT_NAME_STATE_UNMANAGED)));
    assert!(both[1].reason.contains("its holder publishes"), "{}", both[1].reason);
    holds(&zone, &[("shop.example.com", "2001:db8::7"), ("shop.example.com", "203.0.113.7")]).await;

    readdress(&h, "203.0.113.9").await;
    holds(&zone, &[("shop.example.com", "203.0.113.9")]).await;

    // The endpoint stops routing the name its provider published.
    let fewer = web(&push_paper().await, &["shop.example.org"]);
    let changed = change("c1", &shop.id, shop.version, fewer);
    h.workloads(h.root()).declare_intent(changed).await.unwrap();
    let first_plan = created.plan.as_option().unwrap().plan.clone();
    let plan = |a: &agent::Assignment| a.plan.as_option().is_some_and(|p| p.plan != first_plan);
    let moving = until(&mut s, assignment(plan)).await;
    complete(&mut s, &shop.id, moving.plan.as_option().unwrap(), seq + 1).await;
    shown(&h, &shop.id, |n| n.len() == 1).await;
    holds(&zone, &[]).await;
}
