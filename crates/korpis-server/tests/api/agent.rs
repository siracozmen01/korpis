// SPDX-License-Identifier: AGPL-3.0-or-later

//! An agent as a test plays one: it enrolls with a token and its own key, then opens the
//! control stream with a signed Hello (§7 of docs/02-architecture.md).

use crate::support::{Harness, mutation};
use buffa_types::google::protobuf::Timestamp;
use connectrpc::ConnectError;
use connectrpc::client::{BidiStream, HttpClient};
use korpis_core::node::identity::{hello_message, sign};
use korpis_core::tenancy::quota::Bounds;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::node::node_to_pb;
use std::time::{SystemTime, UNIX_EPOCH};

pub type Stream = BidiStream<
    <HttpClient as connectrpc::client::ClientTransport>::ResponseBody,
    agent::AgentMessage,
    agent::__buffa::view::ControlMessageView<'static>,
>;

pub struct TestAgent {
    pub key: ed25519_dalek::SigningKey,
    pub node: String,
}

pub fn agents(h: &Harness) -> agent::AgentServiceClient<HttpClient> {
    agent::AgentServiceClient::new(h.http.clone(), h.config(None))
}

/// The node an agent reports: the `oci` driver and the fixtures' capacity.
fn reported() -> pb::Node {
    let org = uuid::Uuid::now_v7();
    let node = korpis_store::testing::ready_node(org, "reported", &[]).unwrap();
    node_to_pb(&node, &Bounds::default())
}

impl TestAgent {
    /// Enrolls a node named `name` into the root organization with a fresh key.
    pub async fn enroll(h: &Harness, name: &str, seed: u8) -> Self {
        let created = h
            .nodes(h.root())
            .create_enrollment_token(pb::CreateEnrollmentTokenRequest {
                mutation: mutation(&format!("enroll-{name}")).into(),
                organization: h.root_org(),
                node_name: name.to_owned(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let enrolled = agents(h)
            .enroll(agent::EnrollRequest {
                enrollment_token: created.token,
                node_public_key: key.verifying_key().to_bytes().to_vec(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned();
        Self { key, node: enrolled.node }
    }

    /// A Hello signed at `at` with `nonce`.
    pub fn hello_at(&self, at: SystemTime, nonce: &[u8]) -> agent::AgentMessage {
        let since = at.duration_since(UNIX_EPOCH).unwrap();
        let (seconds, nanos) = (i64::try_from(since.as_secs()).unwrap(), since.subsec_nanos());
        let signature = sign(&self.key, &hello_message(&self.node, seconds, nanos, nonce));
        let node = reported();
        let hello = agent::Hello {
            node: self.node.clone(),
            agent_version: "0.1.0-test".into(),
            protocol: "korpis.agent.v1".into(),
            drivers: node.drivers,
            capacity: node.capacity,
            allocatable: node.allocatable,
            auth: agent::NodeAuth {
                timestamp: Timestamp {
                    seconds,
                    nanos: i32::try_from(nanos).unwrap(),
                    ..Default::default()
                }
                .into(),
                nonce: nonce.to_vec(),
                signature,
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        agent::AgentMessage { message: Some(Up::Hello(Box::new(hello))), ..Default::default() }
    }

    /// A heartbeat.
    pub fn heartbeat() -> agent::AgentMessage {
        let node = reported();
        let status = agent::NodeStatus {
            capacity: node.capacity,
            allocatable: node.allocatable,
            drivers: node.drivers,
            ..Default::default()
        };
        agent::AgentMessage {
            message: Some(Up::NodeStatus(Box::new(status))),
            ..Default::default()
        }
    }

    /// Opens a session with `hello`; the stream, and the first answer.
    pub async fn open(
        h: &Harness,
        hello: agent::AgentMessage,
    ) -> (Stream, Result<agent::ControlMessage, ConnectError>) {
        let mut stream = agents(h).session().await.unwrap();
        stream.send(hello).await.unwrap();
        let first = match stream.message::<agent::ControlMessage>().await {
            Ok(Some(m)) => Ok(m.to_owned_message()),
            Ok(None) => Err(ConnectError::internal("the stream ended")),
            Err(e) => Err(e),
        };
        (stream, first)
    }
}

/// The Welcome in a first answer.
pub fn welcome(m: &agent::ControlMessage) -> agent::Welcome {
    match m.message.as_ref() {
        Some(Down::Welcome(w)) => (**w).clone(),
        other => panic!("expected a Welcome, got {other:?}"),
    }
}
