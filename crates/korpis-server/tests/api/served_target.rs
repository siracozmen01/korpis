// SPDX-License-Identifier: AGPL-3.0-or-later

//! A backup target that serves its store itself (§5.6 of docs/06-storage.md): it names the
//! endpoint it serves on rather than an address, and the control plane answers every reader with
//! that endpoint's published address and the target's certificate; an endpoint with no published
//! address is unreachable, and an answer naming both an address and an endpoint is malformed.

use crate::extensions::{DNS, push_document};
use crate::providers::installed_from;
use crate::support::Harness;
use korpis_app::extension::CallTarget;
use korpis_core::extension::{AccessMode, CallOutcome, ProviderType};
use korpis_extension::backup::{Backup, BackupTarget, Failure};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::object_access::Protocol;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// A target serving its own store on the endpoint `store`; under `away/` it names an endpoint its
/// workload has not, and under `both/` it names an address as well.
struct Serving;

impl BackupTarget for Serving {
    fn grant_access(
        &self,
        prefix: String,
        _mode: pb::AccessMode,
        _ttl: Duration,
    ) -> impl Future<Output = Result<pb::ObjectAccess, Failure>> + Send {
        let served_by = if prefix.starts_with("away/") { "away" } else { "store" };
        let endpoint = if prefix.starts_with("both/") { "https://s3.example" } else { "" };
        let s3 = pb::S3Access {
            endpoint: endpoint.into(),
            bucket: "korpis".into(),
            key_prefix: prefix,
            access_key_id: "KF".into(),
            secret_access_key: "minted".into(),
            session_token: "token".into(),
            path_style: true,
            served_by: served_by.into(),
            certificate: b"the target's own certificate".to_vec(),
            ..Default::default()
        };
        std::future::ready(Ok(pb::ObjectAccess {
            protocol: Some(Protocol::S3(Box::new(s3))),
            ..Default::default()
        }))
    }

    fn list(
        &self,
        _: String,
        _: String,
        _: u32,
    ) -> impl Future<Output = Result<pb::ObjectList, Failure>> + Send {
        std::future::ready(Ok(pb::ObjectList::default()))
    }

    fn delete(&self, _: Vec<String>) -> impl Future<Output = Result<u32, Failure>> + Send {
        std::future::ready(Ok(0))
    }
}

/// The node port the installation's workload holds for its endpoint `store`, once placed.
async fn published(h: &Harness, workload: &str) -> u32 {
    for _ in 0..100 {
        let get = pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() };
        let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
        let s = got.status.into_option().unwrap();
        let store = s.endpoints.iter().find(|e| e.name == "store");
        if let Some(port) = store.and_then(|e| e.address.as_option()).map(|a| a.port) {
            return port;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{workload}'s endpoint was never published");
}

#[tokio::test]
async fn every_reader_is_given_the_published_address_of_the_endpoint_the_target_names() {
    let h = Harness::new().await;
    let document = DNS
        .replace("    - {type: dns, name: cloudflare}", "    - {type: backup_target, name: disk}")
        .replace("requires: [provider.serve, provider.dns,", "requires: [provider.serve, provider.backup_target,")
        .replace(
            "  console: none\n",
            "  console: none\n  endpoints:\n    - {name: store, protocol: tcp, port: 9000, exposure_default: node_port}\n",
        );
    let x = installed_from(&h, 72, "disk", push_document(&document).await).await;
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_BACKUP_TARGET,
        name: "disk".into(),
        version: "1.0.0".into(),
        concurrency: 4,
        sign_in: false,
    };
    let provider = Arc::new(Provider::new(connection, registration, Backup(Serving)));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
    let org = korpis_core::id::Id::parse(korpis_core::id::IdKind::Organization, &h.root_org());
    let target = CallTarget {
        organization: org.unwrap().uuid(),
        kind: ProviderType::BackupTarget,
        name: "disk".into(),
    };
    let port = published(&h, &x.extension.workload).await;
    let app = h.api.app();
    let ttl = Duration::from_secs(900);

    let called = app.grant_backup_access(&target, "workloads/w1/", AccessMode::Write, ttl).await;
    let CallOutcome::Access(a) = called.unwrap().outcome else { panic!("no access") };
    assert_eq!(a.s3.endpoint, format!("https://203.0.113.10:{port}"), "the node's address");
    assert!(a.s3.served_by.is_empty(), "resolved, so no reader sees the name");
    assert_eq!(a.s3.certificate, b"the target's own certificate");

    let away = app.grant_backup_access(&target, "away/w1/", AccessMode::Write, ttl).await;
    let CallOutcome::Failed(f) = away.unwrap().outcome else { panic!("an endpoint it has not") };
    assert_eq!(f.reason, "backup.unreachable");
    let both = app.grant_backup_access(&target, "both/w1/", AccessMode::Write, ttl).await;
    let CallOutcome::Failed(f) = both.unwrap().outcome else { panic!("both said") };
    assert_eq!(f.reason, "provider.malformed");
}
