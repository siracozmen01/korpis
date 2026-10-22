// SPDX-License-Identifier: AGPL-3.0-or-later

//! Publishing an order's challenges (§3.2 of docs/07-networking.md), each name its own way. A name
//! is proven by DNS where a DNS provider its organization sees serves its zone: the providers are
//! asked in turn, the organization's own before its ancestors', until one publishes the TXT
//! record, a provider that manages no zone holding the name answering `dns.zone_not_found`. The
//! record is applied for an owner of its own, the certificate and the name's place among its
//! names, so it never disturbs an endpoint's records, and it is recorded before the provider is
//! asked, so it is withdrawn even if this issuer stops mid-order. A name no provider publishes is
//! proven through the nodes its endpoint is placed on, over HTTP-01 or by TLS-ALPN-01, whichever
//! way the order is to try (`NodeProof::for_attempt`), or the other where the directory does not
//! offer that one.
//!
//! The directory is told to look at a DNS record once the control plane's own resolver finds it,
//! or after two minutes if it never does: a provider answers once its zone is changed, which its
//! authoritative servers may not yet serve, and a directory that looks too early fails the name.

use super::issue::{CALL_WITHIN, Order};
use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{
    AcmeError, CertificateWrite as _, Challenge, Clock, HttpOffer, PortalWrite as _, Proof,
    RecipeSource, Store, Tx as _,
};
use korpis_core::dns::certificate::{AlpnChallenge, NodeProof};
use korpis_core::dns::{DnsRecord, RecordType};
use korpis_core::extension::{CallOutcome, ProviderType};
use korpis_core::id::{Id, IdKind};
use std::time::Duration;
use uuid::Uuid;

/// How long the nodes serving a certificate's names have to acknowledge its challenges.
const HELD_WITHIN: Duration = Duration::from_secs(60);
/// How long a node's proxy is given, past its node's acknowledgement, to answer them.
const GRACE: Duration = Duration::from_secs(2);
/// How often the issuer looks whether the nodes hold the challenges.
const POLL: Duration = Duration::from_millis(500);
/// How long the issuer waits for its resolver to find a challenge's TXT record.
const PROPAGATED_WITHIN: Duration = Duration::from_secs(120);
/// How often it asks.
const PROPAGATION_POLL: Duration = Duration::from_secs(2);
/// A challenge record's time to live: short, since it is withdrawn within minutes.
const RECORD_TTL: u32 = 60;
/// What a DNS provider answers for a name in no zone it manages.
const ZONE_NOT_FOUND: &str = "dns.zone_not_found";

fn unavailable(e: impl std::fmt::Display) -> AcmeError {
    AcmeError::Unavailable(e.to_string())
}

/// Why `name` could not be proven: no DNS provider serves its zone, and the directory did not
/// offer the challenge the rest is answered by, TLS-ALPN-01 for a portal, which the control
/// plane's listener answers, and HTTP-01 for an endpoint, which its node's proxy answers.
fn unprovable(portal: bool, name: &str) -> String {
    if portal {
        format!(
            "no DNS provider of its organization serves the zone of {name}, and TLS-ALPN-01 is not \
             offered"
        )
    } else {
        format!(
            "no DNS provider serves the zone of {name}, and neither HTTP-01 nor TLS-ALPN-01 is \
             offered"
        )
    }
}

/// How a node's proxy proves `c`: `way`, or the other where the directory does not offer it.
fn at_node(way: NodeProof, c: &Challenge) -> Option<NodeProof> {
    let offered = |w| match w {
        NodeProof::Http01 => c.http.is_some(),
        NodeProof::TlsAlpn01 => c.tls_alpn.is_some(),
    };
    [way, way.other()].into_iter().find(|w| offered(*w))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The DNS providers `organization` sees, by name, its own first: one of an ancestor of the
    /// same name is shadowed, as calls are routed (§3.3 of docs/16-extensions.md).
    pub(crate) async fn dns_providers(&self, organization: Uuid) -> Result<Vec<String>, AppError> {
        let mut names: Vec<String> = Vec::new();
        for org in self.organization_chain(organization).await? {
            let mut own: Vec<String> = self
                .store
                .provides_in(org)
                .await?
                .into_iter()
                .filter(|p| p.kind == ProviderType::Dns && !names.contains(&p.name))
                .map(|p| p.name)
                .collect();
            own.sort();
            own.dedup();
            names.extend(own);
        }
        Ok(names)
    }

    /// Publishes one challenge of each name, answering how each is proven.
    pub(super) async fn publish(
        &self,
        order: &Order,
        challenges: Vec<Challenge>,
    ) -> Result<Vec<Proof>, AcmeError> {
        let mut proofs = Vec::with_capacity(challenges.len());
        let (mut http, mut alpn, mut txt) = (Vec::new(), Vec::new(), Vec::new());
        for (index, c) in challenges.into_iter().enumerate() {
            let node = at_node(order.way, &c);
            let by_dns = match &c.dns_value {
                Some(value) => self.publish_txt(order, index, &c.name, value).await?,
                None => false,
            };
            if let (true, Some(value)) = (by_dns, c.dns_value) {
                proofs.push(Proof::Dns01);
                txt.push((c.name, value));
                continue;
            }
            if order.portal {
                proofs.push(self.answer_alpn(order.id, &c.name, c.tls_alpn).await?);
                continue;
            }
            match (node, c.http, c.tls_alpn) {
                (Some(NodeProof::Http01), Some(offer), _) => {
                    http.push((c.name, offer));
                    proofs.push(Proof::Http01);
                    let _ = order.proved.set(NodeProof::Http01);
                }
                (Some(NodeProof::TlsAlpn01), _, Some(digest)) => {
                    alpn.push(AlpnChallenge { name: c.name, digest });
                    proofs.push(Proof::TlsAlpn01);
                    let _ = order.proved.set(NodeProof::TlsAlpn01);
                }
                _ => return Err(AcmeError::Refused(unprovable(false, &c.name))),
            }
        }
        if !http.is_empty() || !alpn.is_empty() {
            self.publish_nodes(order, &http, &alpn).await?;
        }
        self.propagated(&txt).await?;
        Ok(proofs)
    }

    /// Proves the portal's name `name` by TLS-ALPN-01 with `digest`, its DNS not having proven it.
    async fn answer_alpn(
        &self,
        certificate: Uuid,
        name: &str,
        digest: Option<Vec<u8>>,
    ) -> Result<Proof, AcmeError> {
        let Some(digest) = digest else {
            return Err(AcmeError::Refused(unprovable(true, name)));
        };
        self.publish_alpn(certificate, name, &digest).await.map_err(unavailable)?;
        Ok(Proof::TlsAlpn01)
    }

    /// Records the TLS-ALPN-01 digest the order of `certificate` answers `name` with, which
    /// whichever replica the directory reaches reads for its handshake.
    async fn publish_alpn(
        &self,
        certificate: Uuid,
        name: &str,
        digest: &[u8],
    ) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.put_portal_challenge(name, certificate, digest).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Withdraws the TLS-ALPN-01 digests the order of `certificate` recorded, however it ended.
    pub(super) async fn withdraw_alpn(&self, certificate: Uuid) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.clear_portal_challenges(certificate).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Asks each DNS provider in turn to publish `name`'s challenge record; whether one did.
    async fn publish_txt(
        &self,
        order: &Order,
        index: usize,
        name: &str,
        value: &str,
    ) -> Result<bool, AcmeError> {
        let owner = format!("{}/{index}", Id::from_uuid(IdKind::Certificate, order.id));
        let record = DnsRecord {
            name: format!("_acme-challenge.{name}"),
            kind: RecordType::Txt,
            ttl: RECORD_TTL,
            values: vec![value.to_owned()],
        };
        for provider in &order.providers {
            self.note_record(order.id, provider, &owner, true).await.map_err(unavailable)?;
            let target = CallTarget {
                organization: order.organization,
                kind: ProviderType::Dns,
                name: provider.clone(),
            };
            let called = self
                .apply_dns_records(&target, &owner, vec![record.clone()], CALL_WITHIN)
                .await
                .map_err(unavailable)?;
            let published = match called.outcome {
                CallOutcome::RecordsApplied { .. } => return Ok(true),
                // A failure that may have published it is withdrawn later; any other is
                // forgotten now.
                CallOutcome::Failed(f) => f.reason != ZONE_NOT_FOUND && f.retryable,
                _ => false,
            };
            if !published {
                self.note_record(order.id, provider, &owner, false).await.map_err(unavailable)?;
            }
        }
        Ok(false)
    }

    async fn note_record(
        &self,
        certificate: Uuid,
        provider: &str,
        owner: &str,
        standing: bool,
    ) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        if standing {
            tx.put_record(certificate, provider, owner).await?;
        } else {
            tx.drop_record(certificate, provider, owner).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Carries the HTTP-01 challenges `http` and the TLS-ALPN-01 ones `alpn` to the order's
    /// nodes through their assignments, answering once every one has acknowledged a set carrying
    /// them, and its proxy has had a moment to take them.
    async fn publish_nodes(
        &self,
        order: &Order,
        http: &[(String, HttpOffer)],
        alpn: &[AlpnChallenge],
    ) -> Result<(), AcmeError> {
        if order.nodes.is_empty() {
            let why = "its endpoint is placed on no node, and no DNS provider serves its zone";
            return Err(AcmeError::Unavailable(why.into()));
        }
        let store = |e: AppError| unavailable(e);
        let mut tx = self.store.begin().await.map_err(|e| store(e.into()))?;
        tx.put_challenges(order.id, http).await.map_err(|e| store(e.into()))?;
        tx.put_alpn_challenges(order.id, alpn).await.map_err(|e| store(e.into()))?;
        tx.commit().await.map_err(|e| store(e.into()))?;
        let timer = self.timer.clone().ok_or_else(|| unavailable("no timer to wait with"))?;
        let mut waited = Duration::ZERO;
        loop {
            let mut held = true;
            for &node in &order.nodes {
                let set = self.sync_assignments(node).await.map_err(store)?;
                let acked = self.store.acked_set(node).await.map_err(|e| store(e.into()))?;
                held &= acked.as_deref() == Some(set.digest.as_str());
            }
            if held {
                timer.sleep(GRACE).await;
                return Ok(());
            }
            if waited >= HELD_WITHIN {
                let why = "the node serving the names did not take the challenges within a minute";
                return Err(AcmeError::Unavailable(why.into()));
            }
            timer.sleep(POLL).await;
            waited += POLL;
        }
    }

    /// Waits until the control plane's resolver finds each published TXT record, or for at most
    /// [`PROPAGATED_WITHIN`].
    async fn propagated(&self, published: &[(String, String)]) -> Result<(), AcmeError> {
        let (Some(lookup), false) = (self.lookup.clone(), published.is_empty()) else {
            return Ok(());
        };
        let timer = self.timer.clone().ok_or_else(|| unavailable("no timer to wait with"))?;
        let mut waited = Duration::ZERO;
        for (name, value) in published {
            let at = format!("_acme-challenge.{name}");
            let found = |f: Vec<String>| f.contains(value);
            while waited < PROPAGATED_WITHIN && !lookup.txt(&at).await.is_ok_and(found) {
                timer.sleep(PROPAGATION_POLL).await;
                waited += PROPAGATION_POLL;
            }
        }
        Ok(())
    }

    /// Withdraws the challenge records standing for `certificate`, or, without one, for every
    /// certificate no issuer holds; how many were withdrawn. One whose provider cannot be reached
    /// stays recorded, and is withdrawn by a later sweep.
    pub(super) async fn withdraw_records(
        &self,
        certificate: Option<Uuid>,
    ) -> Result<u64, AppError> {
        let mut tx = self.store.begin().await?;
        let standing = tx.standing_records(certificate, self.clock.now()).await?;
        tx.commit().await?;
        let mut withdrawn = 0;
        for r in standing {
            let target = CallTarget {
                organization: r.organization,
                kind: ProviderType::Dns,
                name: r.provider.clone(),
            };
            let called = self.apply_dns_records(&target, &r.owner, Vec::new(), CALL_WITHIN).await?;
            let gone = match called.outcome {
                CallOutcome::RecordsApplied { .. } => true,
                CallOutcome::Failed(f) => !f.retryable,
                _ => false,
            };
            if gone {
                self.note_record(r.certificate, &r.provider, &r.owner, false).await?;
                withdrawn += 1;
            }
        }
        Ok(withdrawn)
    }

    /// Withdraws the challenge records an issuer that stopped mid-order left standing; how many.
    pub async fn withdraw_stale_records(&self) -> Result<u64, AppError> {
        self.withdraw_records(None).await
    }
}
