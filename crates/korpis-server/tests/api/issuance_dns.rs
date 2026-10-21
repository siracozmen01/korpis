// SPDX-License-Identifier: AGPL-3.0-or-later

//! DNS-01 through the holder's DNS providers (§3.2 of docs/07-networking.md), validated for real:
//! Pebble resolves `_acme-challenge` records through a DNS server of the test's, the zone a DNS
//! provider of the organization publishes into. Of two providers, the first manages no zone
//! holding the name and says so; the second publishes the record, and the certificate is issued
//! with no node ever acknowledging a challenge; the record is withdrawn once it is.

use crate::agent::{Stream, TestAgent, welcome};
use crate::extensions::{DNS, push_document};
use crate::providers::{Installed, installed_from};
use crate::recipes::{project, push_paper};
use crate::routes::{attest, declare, web};
use crate::support::Harness;
use crate::zone::Zone;
use korpis_extension::dns::{Dns, DnsProvider, Failure};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::v1 as pb;
use korpis_server::testing::PebbleOptions;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// A DNS provider publishing TXT records into `zone` for names under `example.com`, or refusing
/// every name as in no zone it manages.
#[derive(Clone)]
struct Publishing {
    zone: Option<Zone>,
    owned: Arc<Mutex<BTreeMap<String, Vec<String>>>>,
}

impl Publishing {
    fn apply_now(
        &self,
        owner: &str,
        records: &[pb::DnsRecord],
    ) -> Result<pb::RecordsApplied, Failure> {
        let refuse = Failure {
            reason: "dns.zone_not_found".into(),
            message: String::new(),
            retryable: false,
        };
        let Some(zone) = &self.zone else { return Err(refuse) };
        if records.iter().any(|r| !r.name.ends_with(".example.com")) {
            return Err(refuse);
        }
        let mut held = self.owned.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = held.remove(owner).unwrap_or_default();
        for name in &before {
            zone.withdraw(name);
        }
        for r in records {
            for v in &r.values {
                zone.publish(&r.name, v);
            }
        }
        held.insert(owner.to_owned(), records.iter().map(|r| r.name.clone()).collect());
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        Ok(pb::RecordsApplied {
            created: count(records.len()),
            deleted: count(before.len()),
            ..Default::default()
        })
    }
}

impl DnsProvider for Publishing {
    fn apply(
        &self,
        owner: String,
        records: Vec<pb::DnsRecord>,
    ) -> impl Future<Output = Result<pb::RecordsApplied, Failure>> + Send {
        std::future::ready(self.apply_now(&owner, &records))
    }
}

/// Serves `as_name` from `x` with `provider` until the test ends.
async fn serve(h: &Harness, x: &Installed, as_name: &str, provider: Publishing) {
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_DNS,
        name: as_name.into(),
        version: "1.0.0".into(),
        concurrency: 4,
        sign_in: false,
    };
    let provider = Arc::new(Provider::new(connection, registration, Dns(provider)));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
}

/// A node serving the ingress, connected, which never acknowledges anything.
async fn node(h: &Harness) -> Stream {
    let edge = TestAgent::enroll(h, "fra-1", 1).await;
    let (s, first) = TestAgent::open(h, edge.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE node SET ingress = true").execute(store.pool()).await.unwrap();
    s
}

#[tokio::test]
async fn a_name_a_dns_provider_serves_is_proven_by_dns_and_its_record_withdrawn() {
    let (zone, dns) = Zone::serve().await;
    let options = PebbleOptions { dns: Some(dns), ..PebbleOptions::default() };
    let Some(pebble) = crate::pebble::start_with(options).await else { return };
    let h = Harness::with_acme(pebble.config(), Some(dns)).await;
    let elsewhere = installed_from(&h, 51, "elsewhere", push_document(DNS).await).await;
    let refusing = Publishing { zone: None, owned: Arc::default() };
    serve(&h, &elsewhere, "cloudflare", refusing).await;
    let other = DNS.replace("name: cloudflare", "name: zone");
    let serving = installed_from(&h, 52, "zone", push_document(&other).await).await;
    let publishing = Publishing { zone: Some(zone.clone()), owned: Arc::default() };
    serve(&h, &serving, "zone", publishing.clone()).await;

    let _node = node(&h).await;
    let project = project(&h, &h.root_org(), "shops").await;
    attest(&h, "d1", &h.root_org(), "example.com").await;
    declare(&h, &project, "shop", web(&push_paper().await, &["shop.example.com"])).await;

    let listed = pb::ListCertificatesRequest { organization: h.root_org(), ..Default::default() };
    let mut issued = None;
    for _ in 0..600 {
        let certs = h.certificates(h.root()).list_certificates(listed.clone()).await.unwrap();
        let certs = certs.into_owned().certificates;
        let done = pb::CertificateState::CERTIFICATE_STATE_ISSUED;
        if let Some(c) = certs.into_iter().find(|c| c.state.as_known() == Some(done)) {
            issued = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(issued) = issued else {
        let certs = h.certificates(h.root()).list_certificates(listed).await.unwrap();
        panic!("not issued by DNS-01 within a minute: {:?}", certs.into_owned().certificates);
    };
    assert_eq!(issued.names, ["shop.example.com"]);
    let store = h.db.store().await.unwrap();
    let challenges: i64 = sqlx::query_scalar("SELECT count(*) FROM certificate_challenge")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(challenges, 0, "no HTTP-01 challenge was published");
    let standing: i64 = sqlx::query_scalar("SELECT count(*) FROM certificate_record")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(standing, 0, "the record is withdrawn and forgotten");
    // The endpoint's own records stand beside it, under an owner of their own (§3.5).
    let owned = publishing.owned.lock().unwrap().clone();
    let challenge = owned.values().flatten().any(|n| n.starts_with("_acme-challenge."));
    assert!(!challenge, "no challenge record left: {owned:?}");
}
