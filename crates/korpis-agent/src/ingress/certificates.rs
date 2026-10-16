// SPDX-License-Identifier: AGPL-3.0-or-later

//! The certificates the node's proxy serves with (§3.2 of docs/07-networking.md). Each is asked
//! of the control plane up the session when an endpoint placed here names it at a revision the
//! node does not hold, answered only because the endpoint is placed here, and kept in the state
//! directory, readable by the agent alone, so a restart while the control plane is unreachable
//! still serves. One no endpoint here names any more is removed, key and all.
//!
//! Asking never holds the reconciler's loop: a pass asks and moves on, and a later pass finds the
//! answer. A question unanswered for a minute is taken as lost, and asked again; a refusal is
//! asked again after ten seconds, since what it waits on (a placement the control plane has not
//! yet recorded, a certificate not yet issued) passes.

use buffa::Message as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::certificate_key::Outcome;
use korpis_proto::korpis::ingress::v1 as ingress;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long an unanswered question stands before it is asked again.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);
/// How long a refused question waits before it is asked again.
const AFTER_REFUSAL: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
struct Held {
    /// By certificate identifier.
    kept: BTreeMap<String, agent::ServedCertificate>,
    /// Questions standing, by certificate: the request and when it was sent or refused.
    asked: BTreeMap<String, (String, Instant, Duration)>,
}

/// The certificates the node holds, and those it has asked for.
#[derive(Debug)]
pub struct Certificates {
    dir: PathBuf,
    outbound: mpsc::Sender<agent::AgentMessage>,
    held: Mutex<Held>,
}

/// Whether `id` can name a file: an identifier as the control plane writes them.
fn plain(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Certificates {
    /// Keeps certificates in `dir`, asking for them up `outbound`; those kept there already are
    /// served at once.
    #[must_use]
    pub fn open(dir: PathBuf, outbound: mpsc::Sender<agent::AgentMessage>) -> Arc<Self> {
        let mut kept = BTreeMap::new();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "binpb") {
                continue;
            }
            match std::fs::read(&path).map(|b| agent::ServedCertificate::decode_from_slice(&b)) {
                Ok(Ok(c)) if plain(&c.id) => {
                    kept.insert(c.id.clone(), c);
                }
                _ => tracing::warn!(path = %path.display(), "a kept certificate does not read"),
            }
        }
        let held = Mutex::new(Held { kept, asked: BTreeMap::new() });
        Arc::new(Self { dir, outbound, held })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks for certificate `id` at `revision`, unless it is held at that revision or later, or
    /// a question for it stands.
    pub fn want(&self, id: &str, revision: i64) {
        let mut held = self.lock();
        if held.kept.get(id).is_some_and(|c| c.revision >= revision) {
            return;
        }
        if held.asked.get(id).is_some_and(|(_, at, wait)| at.elapsed() < *wait) {
            return;
        }
        let request = uuid::Uuid::now_v7().to_string();
        let up = agent::CertificateRequest {
            request: request.clone(),
            certificate: id.to_owned(),
            ..Default::default()
        };
        let message = agent::AgentMessage {
            message: Some(Up::CertificateRequest(Box::new(up))),
            ..Default::default()
        };
        if self.outbound.try_send(message).is_ok() {
            held.asked.insert(id.to_owned(), (request, Instant::now(), ANSWER_WITHIN));
        }
    }

    /// Keeps an answer to a question that stands.
    pub fn answer(&self, answer: agent::CertificateKey) {
        let mut held = self.lock();
        let id = held.asked.iter().find(|(_, (r, _, _))| *r == answer.request).map(|(id, _)| id);
        let Some(id) = id.cloned() else { return };
        match answer.outcome {
            Some(Outcome::Certificate(c)) if c.id == id && plain(&c.id) => {
                held.asked.remove(&id);
                let path = self.dir.join(format!("{id}.binpb"));
                let kept = std::fs::create_dir_all(&self.dir)
                    .and_then(|()| crate::durable::replace(&path, &c.encode_to_vec()));
                if let Err(e) = kept {
                    tracing::warn!(error = %e, certificate = %id, "served, but not kept on disk");
                }
                held.kept.insert(id, *c);
            }
            Some(Outcome::Refused(e)) => {
                tracing::info!(certificate = %id, reason = %e.reason, "a certificate waits");
                held.asked.insert(id, (answer.request, Instant::now(), AFTER_REFUSAL));
            }
            _ => {}
        }
    }

    /// The certificates of `ids` the node holds, as the proxy is given them; every other one is
    /// removed, from memory and from disk.
    pub fn serve(&self, ids: &BTreeSet<String>) -> Vec<ingress::Certificate> {
        let mut held = self.lock();
        let gone: Vec<String> = held.kept.keys().filter(|k| !ids.contains(*k)).cloned().collect();
        for id in gone {
            held.kept.remove(&id);
            let _ = std::fs::remove_file(self.dir.join(format!("{id}.binpb")));
        }
        held.asked.retain(|id, _| ids.contains(id));
        held.kept
            .values()
            .map(|c| ingress::Certificate {
                id: c.id.clone(),
                chain: c.chain.clone(),
                key: c.key.clone(),
                ..Default::default()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
