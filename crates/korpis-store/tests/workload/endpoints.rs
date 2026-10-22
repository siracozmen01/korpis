// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints as placed (migrations 0053, 0056, 0118 and 0119, §3.2 of docs/07-networking.md):
//! an ingress endpoint reads back with where its certificate comes from and the client
//! certificates it asks for, and one whose TLS is passed through with whether its workload is
//! sent a PROXY header; neither may be stored on an endpoint of the other kind.

use crate::support::{create, fixture, now};
use korpis_app::ports::{EndpointRead as _, EndpointWrite as _, Store as _, Tx as _};
use korpis_core::workload::endpoint::Endpoint;
use korpis_core::workload::spec::endpoints::{
    ClientAuth, ClientCertificates, Exposure, Protocol, TlsSpec,
};
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

#[tokio::test]
async fn a_terminated_endpoint_reads_back_with_the_client_certificates_it_asks_for() {
    let f = fixture().await;
    let (w, _) = create(&f, "locked").await;
    let node = ready_node(f.organization, "fra-6", &[]).unwrap();
    enroll(&f.store, &node).await.unwrap();
    let clients = |certificates| {
        Some(ClientAuth { certificates, authorities: vec![vec![0x30, 1, 2], vec![0x30, 3]] })
    };
    let api = ingress(
        w.id,
        node.id,
        "api",
        TlsSpec::Acme { clients: clients(ClientCertificates::Required) },
    );
    let tls = TlsSpec::Provided {
        secret: "crt_x".into(),
        clients: clients(ClientCertificates::Optional),
    };
    let portal = ingress(w.id, node.id, "portal", tls);
    let mut tx = f.store.begin().await.unwrap();
    tx.put_endpoint(&api, now()).await.unwrap();
    tx.put_endpoint(&portal, now()).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.store.endpoints(w.id).await.unwrap(), [api.clone(), portal]);

    for refused in [
        "UPDATE endpoint SET tls = 'passthrough' WHERE id = $1",
        "UPDATE endpoint SET client_ca = '{}' WHERE id = $1",
        "UPDATE endpoint SET client_certificates = NULL WHERE id = $1",
    ] {
        let done = sqlx::query(refused).bind(api.id).execute(f.store.pool()).await;
        assert!(done.is_err(), "{refused}");
    }
}
