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
    /// The control plane's assignment verification keys, as enrollment returned them.
    pub assignment_keys: Vec<agent::SigningKey>,
    /// Its capability-token verification keys, as enrollment returned them.
    pub token_keys: Vec<agent::SigningKey>,
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
        // An enrolled node is edge-only until the prober measures it (§9 of
        // docs/07-networking.md), and this hand-played node advertises nothing of its own. The
        // recipes these tests place have a `stable` endpoint, which without an edge tier needs a
        // public node with a free port in its range, so this stands in for both: an address in
        // TEST-NET-3 with the default range, measured now so the prober leaves it be.
        let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Node, &enrolled.node).unwrap();
        let store = h.db.store().await.unwrap();
        sqlx::query(
            "UPDATE node SET reachability = 'public', addresses = '{203.0.113.10}',
                             first_port = 30000, last_port = 32767, probed_at = now()
              WHERE id = $1",
        )
        .bind(id.uuid())
        .execute(store.pool())
        .await
        .unwrap();
        Self {
            key,
            node: enrolled.node,
            assignment_keys: enrolled.assignment_keys,
            token_keys: enrolled.token_keys,
        }
    }

    /// A data connection's first message to `relay`, signed now with `nonce`.
    pub fn data_hello(&self, relay: &str, nonce: &[u8]) -> agent::AttachUp {
        let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let (seconds, nanos) = (i64::try_from(since.as_secs()).unwrap(), since.subsec_nanos());
        let message = korpis_core::node::identity::data_hello_message;
        let signature = sign(&self.key, &message((&self.node, relay), seconds, nanos, nonce));
        let auth = agent::NodeAuth {
            timestamp: Timestamp {
                seconds,
                nanos: i32::try_from(nanos).unwrap(),
                ..Default::default()
            }
            .into(),
            nonce: nonce.to_vec(),
            signature,
            ..Default::default()
        };
        let hello = agent::DataHello {
            node: self.node.clone(),
            auth: auth.into(),
            relay: relay.to_owned(),
            ..Default::default()
        };
        agent::AttachUp {
            message: Some(agent::__buffa::oneof::attach_up::Message::Hello(Box::new(hello))),
            ..Default::default()
        }
    }

    /// A Hello signed at `at` with `nonce`.
    pub fn hello_at(&self, at: SystemTime, nonce: &[u8]) -> agent::AgentMessage {
        self.hello_with(at, nonce, Vec::new())
    }

    /// A hello at `at` declaring the storage classes `storage`.
    pub fn hello_with(
        &self,
        at: SystemTime,
        nonce: &[u8],
        storage: Vec<pb::StorageClassInfo>,
    ) -> agent::AgentMessage {
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
            storage_classes: storage,
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

/// The next message on a session, within five seconds, passing over revocation lists and the
/// relays named, which arrive on their own schedule (§6.3 of docs/08-identity.md, §3.1 of
/// docs/02-architecture.md); [`next_list`] and [`next_relays`] read those.
pub async fn next(stream: &mut Stream) -> agent::ControlMessage {
    loop {
        let m = any(stream).await;
        if !matches!(
            m.message,
            Some(Down::RevocationList(_) | Down::Relays(_) | Down::TokenKeys(_))
        ) {
            return m;
        }
    }
}

/// The next relays named on a session, within five seconds of each message before it.
pub async fn next_relays(stream: &mut Stream) -> agent::Relays {
    loop {
        if let Some(Down::Relays(relays)) = any(stream).await.message {
            return *relays;
        }
    }
}

/// The next revocation list on a session, within five seconds of each message before it.
pub async fn next_list(stream: &mut Stream) -> agent::RevocationList {
    loop {
        if let Some(Down::RevocationList(list)) = any(stream).await.message {
            return *list;
        }
    }
}

pub async fn any(stream: &mut Stream) -> agent::ControlMessage {
    let received = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.message::<agent::ControlMessage>(),
    )
    .await
    .expect("a message within five seconds");
    received.unwrap().expect("the stream is open").to_owned_message()
}
