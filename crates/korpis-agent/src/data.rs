// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's data connections (§3.1 of docs/02-architecture.md): one dialled out to each relay
//! the control plane names, its own address among them when it serves the relay role. Each
//! attaches with a `DataHello` naming the relay dialled, then opens every session the relay asks for, after judging it again from
//! what the client sent: the token, offline, against the token keys and the revocation list the
//! node holds; the workload, against the set the node holds; the action, against the handlers
//! the node runs. A node never sees the client's address, and never evaluates it.

mod carry;
pub mod handler;

use crate::held::HeldSet;
use crate::state::Identity;
use crate::tls;
use buffa_types::google::protobuf::Timestamp;
use connectrpc::client::{BidiRecvHalf, ClientConfig, ClientTransport, HttpClient};
use handler::{Handler, Handlers, Opened};
use korpis_core::authz::revocation::RevocationList;
use korpis_core::authz::token::{Client, KeyRing, Verified, verify};
use korpis_core::authz::{Kind, ObjectRef, Resource};
use korpis_core::id::IdKind;
use korpis_core::node::identity::{data_hello_message, sign};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::attach_down::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::attach_up::Message as Up;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// The longest wait between attempts to attach.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

type Recv = BidiRecvHalf<
    <HttpClient as ClientTransport>::ResponseBody,
    agent::__buffa::view::AttachDownView<'static>,
>;

/// What the data connections judge sessions by.
#[derive(Debug, Clone)]
pub struct Judged {
    /// The token keys enrollment returned.
    pub keys: KeyRing,
    /// The revocation list held.
    pub revoked: watch::Receiver<Arc<RevocationList>>,
    /// The assignment set held.
    pub held: watch::Receiver<HeldSet>,
    /// What runs each action.
    pub handlers: Handlers,
}

/// The node's data connections.
#[derive(Debug)]
pub struct DataLink {
    identity: Arc<Identity>,
    judged: Arc<Judged>,
    relays: watch::Receiver<Vec<String>>,
}

fn refusal(session: &str, code: &str, message: String) -> agent::AttachUp {
    let refused = agent::OpenRefused {
        session: session.to_owned(),
        code: code.to_owned(),
        message,
        ..Default::default()
    };
    agent::AttachUp { message: Some(Up::Refused(Box::new(refused))), ..Default::default() }
}

impl Judged {
    /// Verifies `token` offline for `action` on `workload`, as a verifier that never sees the
    /// client.
    fn check(&self, token: &str, action: &str, workload: &str) -> Result<Verified, String> {
        let id =
            korpis_wire::id(IdKind::Workload, "workload", workload).map_err(|e| e.to_string())?;
        let on = Resource::alone(ObjectRef::new(Kind::Workload, id));
        let revoked: HashSet<_> = self.revoked.borrow().grants.iter().copied().collect();
        let at = (SystemTime::now(), Client::Unseen);
        verify(token, &self.keys, action, &on, &revoked, at).map_err(|e| e.to_string())
    }

    /// A renewal of `opened`: the same action on the same workload, for the same subject.
    pub(super) fn verify(&self, token: &str, opened: &Opened) -> Result<Verified, String> {
        let verified = self.check(token, &opened.action, &opened.workload)?;
        if verified.subject != opened.subject {
            return Err("a renewal must be the same subject's".to_owned());
        }
        Ok(verified)
    }

    /// The session, with what serves it, if this node accepts it; the refusal to send if not.
    fn admit(
        &self,
        open: &agent::OpenSession,
    ) -> Result<(Arc<dyn Handler>, Opened), agent::AttachUp> {
        let refuse = |code, message| refusal(&open.session, code, message);
        let verified = self
            .check(&open.token, &open.action, &open.workload)
            .map_err(|e| refuse("refused", e))?;
        if !self.held.borrow().assignments.contains_key(&open.workload) {
            return Err(refuse("not_here", "this node does not hold that workload".into()));
        }
        let handler = self.handlers.get(&open.action).ok_or_else(|| {
            refuse("unimplemented", format!("{} is not served here", open.action))
        })?;
        let opened = Opened {
            session: open.session.clone(),
            workload: open.workload.clone(),
            action: open.action.clone(),
            params: open.params.clone(),
            subject: verified.subject,
            chain: verified.chain,
            expires_at: verified.expires_at,
        };
        Ok((handler, opened))
    }
}

/// The node's `DataHello` to `relay`, signed now.
fn hello(identity: &Identity, relay: &str) -> Result<agent::AttachUp, String> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
    let nanos = since.subsec_nanos();
    let signed = data_hello_message((&identity.node, relay), seconds, nanos, &nonce);
    let auth = agent::NodeAuth {
        timestamp: Timestamp {
            seconds,
            nanos: i32::try_from(nanos).unwrap_or(0),
            ..Default::default()
        }
        .into(),
        nonce: nonce.to_vec(),
        signature: sign(&identity.key, &signed),
        ..Default::default()
    };
    let hello = agent::DataHello {
        node: identity.node.clone(),
        auth: auth.into(),
        relay: relay.to_owned(),
        ..Default::default()
    };
    Ok(agent::AttachUp { message: Some(Up::Hello(Box::new(hello))), ..Default::default() })
}

/// Reads the relay's messages into a channel, so the attachment's loop never cancels a read.
async fn read(mut recv: Recv, out: mpsc::Sender<Result<agent::AttachDown, String>>) {
    loop {
        let next = match recv.message::<agent::AttachDown>().await {
            Ok(Some(m)) => Ok(m.to_owned_message()),
            Ok(None) => Err("the relay closed the data connection".to_owned()),
            Err(e) => Err(e.to_string()),
        };
        let ended = next.is_err();
        if out.send(next).await.is_err() || ended {
            return;
        }
    }
}

/// One attachment to `relay`, until it drops.
async fn attach(identity: &Identity, judged: &Arc<Judged>, relay: &str) -> Result<(), String> {
    let config = tls::trusting(&identity.ca_pem).map_err(|e| e.to_string())?;
    let uri: http::Uri = relay.parse().map_err(|e: http::uri::InvalidUri| e.to_string())?;
    let client = agent::DataServiceClient::new(
        HttpClient::with_tls(Arc::new(config)),
        ClientConfig::new(uri),
    );
    let (mut send, recv) = client.attach().await.map_err(|e| e.to_string())?.into_split();
    send.send(hello(identity, relay)?).await.map_err(|e| e.to_string())?;
    let (tx, mut rx) = mpsc::channel(64);
    let reader = tokio::spawn(read(recv, tx));
    let first = rx.recv().await.ok_or("the relay ended before attaching")??;
    if !matches!(first.message, Some(Down::Attached(_))) {
        reader.abort();
        return Err("the relay's first answer was not Attached".to_owned());
    }
    tracing::info!(%relay, "data connection attached");
    let (refusals, mut refused) = mpsc::channel(64);
    let result = loop {
        tokio::select! {
            next = rx.recv() => match next {
                Some(Ok(down)) => {
                    let Some(Down::Open(open)) = down.message else { continue };
                    tokio::spawn(open_session(client.clone(), Arc::clone(judged), *open, refusals.clone()));
                }
                Some(Err(e)) => break Err(e),
                None => break Err("the reader stopped".to_owned()),
            },
            Some(r) = refused.recv() => {
                if let Err(e) = send.send(r).await {
                    break Err(e.to_string());
                }
            }
        }
    };
    reader.abort();
    result
}

/// Judges one session and carries it, or refuses it.
async fn open_session(
    client: agent::DataServiceClient<HttpClient>,
    judged: Arc<Judged>,
    open: agent::OpenSession,
    refusals: mpsc::Sender<agent::AttachUp>,
) {
    match judged.admit(&open) {
        Ok((handler, opened)) => {
            let session = opened.session.clone();
            if let Err(e) = carry::carry(&client, &judged, handler, opened).await {
                tracing::warn!(%session, error = %e, "session ended abnormally");
            }
        }
        Err(refused) => {
            tracing::info!(session = %open.session, "session refused");
            let _ = refusals.send(refused).await;
        }
    }
}

/// Attaches to `relay` for as long as the node runs, backing off between attempts.
async fn attach_forever(identity: Arc<Identity>, judged: Arc<Judged>, relay: String) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let reason = attach(&identity, &judged, &relay).await.err().unwrap_or_default();
        tracing::warn!(%relay, %reason, "data connection down; attaching again in {backoff:?}");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

impl DataLink {
    /// The data connections of `identity`, to the relays `relays` names, judging by `judged`.
    #[must_use]
    pub fn new(identity: Identity, judged: Judged, relays: watch::Receiver<Vec<String>>) -> Self {
        Self { identity: Arc::new(identity), judged: Arc::new(judged), relays }
    }

    /// Makes `running` exactly one connection to each relay in `wanted`.
    fn keep(&self, running: &mut HashMap<String, JoinHandle<()>>, wanted: &[String]) {
        running.retain(|relay, task| {
            wanted.contains(relay) || {
                task.abort();
                false
            }
        });
        for relay in
            wanted.iter().filter(|r| !running.contains_key(*r)).cloned().collect::<Vec<_>>()
        {
            let (identity, judged) = (Arc::clone(&self.identity), Arc::clone(&self.judged));
            running.insert(relay.clone(), tokio::spawn(attach_forever(identity, judged, relay)));
        }
    }

    /// Keeps one connection to each relay named, for as long as the process runs.
    pub async fn run(mut self) {
        let mut running: HashMap<String, JoinHandle<()>> = HashMap::new();
        loop {
            let wanted = self.relays.borrow_and_update().clone();
            self.keep(&mut running, &wanted);
            if self.relays.changed().await.is_err() {
                // The session is gone, and with it any change; the connections stay as they are.
                std::future::pending::<()>().await;
            }
        }
    }
}
