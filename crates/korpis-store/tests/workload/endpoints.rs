// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints as placed (migrations 0053, 0056 and 0118, §3.2 of docs/07-networking.md): an
//! ingress endpoint reads back with where its certificate comes from, and one whose TLS is passed
//! through with whether its workload is sent a PROXY header, which nothing else may carry.

use crate::support::{create, fixture, now};
use korpis_app::ports::{EndpointRead as _, EndpointWrite as _, Store as _, Tx as _};
use korpis_core::workload::endpoint::Endpoint;
use korpis_core::workload::spec::endpoints::{Exposure, Protocol, TlsSpec};
use korpis_store::testing::{enroll, ready_node};
use uuid::Uuid;

fn ingress(workload: Uuid, node: Uuid, name: &str, tls: TlsSpec) -> Endpoint {
    Endpoint {
        id: Uuid::now_v7(),
        workload,
        name: name.into(),
        protocol: Protocol::Tcp,
        container_port: 8443,
        exposure: Exposure::Ingress,
        node,
        port: None,
        domains: vec![format!("{name}.example.com")],
        tls: Some(tls),
        certificate: None,
    }
}

#[tokio::test]
async fn a_passthrough_endpoint_reads_back_with_whether_its_workload_is_sent_a_proxy_header() {
    let f = fixture().await;
    let (w, _) = create(&f, "tunnel").await;
    let node = ready_node(f.organization, "fra-5", &[]).unwrap();
    enroll(&f.store, &node).await.unwrap();
    let told = ingress(w.id, node.id, "told", TlsSpec::Passthrough { proxy_protocol: true });
    let plain = ingress(w.id, node.id, "plain", TlsSpec::Passthrough { proxy_protocol: false });
    let mut tx = f.store.begin().await.unwrap();
    tx.put_endpoint(&told, now()).await.unwrap();
    tx.put_endpoint(&plain, now()).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.store.endpoints(w.id).await.unwrap(), [plain, told.clone()]);

    let refused = sqlx::query("UPDATE endpoint SET tls = 'acme' WHERE id = $1")
        .bind(told.id)
        .execute(f.store.pool())
        .await;
    assert!(refused.is_err(), "a PROXY header is written only where TLS is passed through");
}
