// SPDX-License-Identifier: AGPL-3.0-or-later

//! Publishing records through a DNS provider (§3.5 of docs/07-networking.md): the control plane
//! sends an owner's records whole, a provider written with the SDK makes its zone agree, and what
//! it refuses comes back with the provider's reason.

use crate::providers::installed;
use crate::support::Harness;
use korpis_app::extension::{CallTarget, Called};
use korpis_core::dns::{DnsRecord, RecordType};
use korpis_core::extension::{CallOutcome, ProviderType};
use korpis_extension::dns::{Dns, DnsProvider, Failure};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// Record sets by name and type, each with its owner.
type Sets = BTreeMap<(String, i32), (String, pb::DnsRecord)>;

/// One zone, `example.com`, in memory.
#[derive(Default, Clone)]
struct Zone(Arc<Mutex<Sets>>);

impl Zone {
    fn names(&self) -> Vec<String> {
        self.0.lock().unwrap().keys().map(|(n, _)| n.clone()).collect()
    }

    fn apply_now(
        &self,
        owner: &str,
        records: Vec<pb::DnsRecord>,
    ) -> Result<pb::RecordsApplied, Failure> {
        let refuse = |reason: &str| Failure {
            reason: reason.into(),
            message: String::new(),
            retryable: false,
        };
        if records.iter().any(|r| !r.name.ends_with("example.com")) {
            return Err(refuse("dns.zone_not_found"));
        }
        let mut zone = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = |r: &pb::DnsRecord| (r.name.clone(), r.r#type.to_i32());
        if records.iter().any(|r| zone.get(&key(r)).is_some_and(|(o, _)| o != owner)) {
            return Err(refuse("dns.conflict"));
        }
        let mut applied = pb::RecordsApplied::default();
        let wanted: Vec<_> = records.iter().map(key).collect();
        let before = zone.len();
        zone.retain(|k, (o, _)| o != owner || wanted.contains(k));
        applied.deleted = u32::try_from(before - zone.len()).unwrap_or(u32::MAX);
        for r in records {
            match zone.insert(key(&r), (owner.to_owned(), r.clone())) {
                None => applied.created += 1,
                Some((_, old)) if old != r => applied.updated += 1,
                Some(_) => {}
            }
        }
        Ok(applied)
    }
}

impl DnsProvider for Zone {
    fn apply(
        &self,
        owner: String,
        records: Vec<pb::DnsRecord>,
    ) -> impl Future<Output = Result<pb::RecordsApplied, Failure>> + Send {
        std::future::ready(self.apply_now(&owner, records))
    }
}

fn record(name: &str, kind: RecordType, value: &str) -> DnsRecord {
    DnsRecord { name: name.into(), kind, ttl: 300, values: vec![value.into()] }
}

async fn apply(h: &Harness, owner: &str, records: Vec<DnsRecord>) -> Called {
    let org = korpis_core::id::Id::parse(korpis_core::id::IdKind::Organization, &h.root_org());
    let target = CallTarget {
        organization: org.unwrap().uuid(),
        kind: ProviderType::Dns,
        name: "cloudflare".into(),
    };
    h.api.app().apply_dns_records(&target, owner, records, Duration::from_secs(10)).await.unwrap()
}

#[tokio::test]
async fn an_owners_records_are_made_exactly_so_and_a_refusal_names_why() {
    let h = Harness::new().await;
    let x = installed(&h, 51, "dns").await;
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let zone = Zone::default();
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_DNS,
        name: "cloudflare".into(),
        version: "1.0.0".into(),
        concurrency: 4,
        sign_in: false,
        fencing: pb::Fencing::FENCING_UNSPECIFIED,
    };
    let provider = Arc::new(Provider::new(connection, registration, Dns(zone.clone())));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();

    let first = vec![
        record("play.example.com", RecordType::A, "203.0.113.7"),
        record("_minecraft._tcp.play.example.com", RecordType::Srv, "0 5 25565 play.example.com"),
    ];
    let done = apply(&h, "ep_one", first).await;
    assert_eq!(done.outcome, CallOutcome::RecordsApplied { created: 2, updated: 0, deleted: 0 });
    let narrower = vec![record("play.example.com", RecordType::A, "203.0.113.8")];
    let done = apply(&h, "ep_one", narrower).await;
    assert_eq!(done.outcome, CallOutcome::RecordsApplied { created: 0, updated: 1, deleted: 1 });
    assert_eq!(zone.names(), ["play.example.com"]);

    let theirs = apply(&h, "ep_two", vec![record("play.example.com", RecordType::A, "1.2.3.4")]);
    let CallOutcome::Failed(f) = theirs.await.outcome else { panic!("refused") };
    assert_eq!((f.reason.as_str(), f.retryable), ("dns.conflict", false));
    let elsewhere = apply(&h, "ep_two", vec![record("play.other.org", RecordType::A, "1.2.3.4")]);
    let CallOutcome::Failed(f) = elsewhere.await.outcome else { panic!("refused") };
    assert_eq!(f.reason, "dns.zone_not_found");

    let malformed = vec![record("Play.example.com", RecordType::A, "1.2.3.4")];
    let org = korpis_core::id::Id::parse(korpis_core::id::IdKind::Organization, &h.root_org());
    let target = CallTarget {
        organization: org.unwrap().uuid(),
        kind: ProviderType::Dns,
        name: "cloudflare".into(),
    };
    let refused =
        h.api.app().apply_dns_records(&target, "ep_one", malformed, Duration::from_secs(5)).await;
    assert!(
        matches!(refused, Err(korpis_app::AppError::Invalid { field, .. }) if field == "records[0].name")
    );
}
