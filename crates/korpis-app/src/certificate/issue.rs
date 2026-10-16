// SPDX-License-Identifier: AGPL-3.0-or-later

//! Issuing `acme` certificates (§3.2 of docs/07-networking.md). The issuer takes one certificate
//! due at a time, under a lease no other issuer takes it through, and orders it from the
//! directory. Each name's HTTP-01 challenge is carried to the nodes the endpoint's workload is
//! placed on in their assignments; the directory is told to look only once every such node has
//! acknowledged a set carrying it, and a moment more, its proxy taking challenges from the set it
//! holds rather than from its reconciler's pass. What is issued is read and checked like an upload,
//! its key sealed, and its revision raised, so the nodes fetch it. A failure is recorded with its
//! reason and retried after a backoff; a certificate issued before keeps serving meanwhile.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    AcmeError, CertificateWrite as _, Challenge, Clock, EffectWrite as _, Issued, Proof,
    RecipeSource, Store, Tx as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::dns::certificate::{Certificate, State, backoff, renew_after};
use korpis_core::principal::{Principal, SystemActor};
use std::time::Duration;
use uuid::Uuid;

/// How long an issuer holds the certificate it took: longer than an order takes.
const HOLD: Duration = Duration::from_secs(15 * 60);
/// How long the nodes serving a certificate's names have to acknowledge its challenges.
const HELD_WITHIN: Duration = Duration::from_secs(60);
/// How long a node's proxy is given, past its node's acknowledgement, to answer them.
const GRACE: Duration = Duration::from_secs(2);
/// How often the issuer looks whether the nodes hold the challenges.
const POLL: Duration = Duration::from_millis(500);
/// How long a certificate whose endpoint is placed nowhere waits before it is looked at again.
const UNPLACED: Duration = Duration::from_secs(60);

fn effect(c: &Certificate, outcome: Outcome, reason: Option<&str>) -> Effect {
    Effect::new(Principal::System(SystemActor::Authority), "certificate.issue", outcome).after(
        details([
            ("certificate", c.id.to_string().into()),
            ("names", c.names.join(",").into()),
            ("revision", c.revision.into()),
            ("reason", reason.unwrap_or_default().into()),
        ]),
    )
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Makes every `acme` certificate not issued due at once, as the issuer does when it starts;
    /// how many.
    pub async fn rearm_certificates(&self) -> Result<u64, AppError> {
        let mut tx = self.store.begin().await?;
        let n = tx.rearm_certificates().await?;
        tx.commit().await?;
        Ok(n)
    }

    /// Removes the `acme` certificates nothing names any more; how many.
    pub async fn remove_orphan_certificates(&self) -> Result<u64, AppError> {
        let mut tx = self.store.begin().await?;
        let n = tx.remove_orphans().await?;
        tx.commit().await?;
        Ok(n)
    }

    /// Issues the next `acme` certificate due, if one is; whether one was.
    pub async fn issue_next(&self) -> Result<bool, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let due = tx.take_due(now, now + HOLD).await?;
        tx.commit().await?;
        let Some(c) = due else { return Ok(false) };
        self.issue(c).await?;
        Ok(true)
    }

    /// The nodes the workloads served with certificate `id` are placed on.
    async fn serving(&self, id: Uuid) -> Result<Vec<Uuid>, AppError> {
        let mut nodes = Vec::new();
        for w in self.store.routed_with(id).await? {
            if let Some(p) = self.store.placement(w).await? {
                nodes.push(p.node);
            }
        }
        nodes.sort();
        nodes.dedup();
        Ok(nodes)
    }

    async fn issue(&self, c: Certificate) -> Result<(), AppError> {
        let Some(acme) = self.acme.clone() else {
            let none = AcmeError::Unavailable("the control plane has no ACME directory".into());
            return self.attempt_failed(c, &none).await;
        };
        let nodes = self.serving(c.id).await?;
        if nodes.is_empty() {
            let waiting = Certificate {
                reason: Some("its endpoint is placed on no node yet".into()),
                next_attempt: Some(self.clock.now() + UNPLACED),
                ..c
            };
            return self.record(&waiting, None).await;
        }
        let id = c.id;
        let publish: crate::ports::Publish<'_> =
            Box::new(move |challenges| Box::pin(self.publish(id, nodes, challenges)));
        match acme.issue(&c.names, Proof::Http01, publish).await {
            Ok(issued) => self.issued(c, &issued).await,
            Err(e) => self.attempt_failed(c, &e).await,
        }
    }

    /// Carries `challenges` to `nodes` through their assignments, answering once every one has
    /// acknowledged a set carrying them, and its proxy has had a moment to take them.
    async fn publish(
        &self,
        id: Uuid,
        nodes: Vec<Uuid>,
        challenges: Vec<Challenge>,
    ) -> Result<(), AcmeError> {
        let store = |e: AppError| AcmeError::Unavailable(e.to_string());
        let mut tx = self.store.begin().await.map_err(|e| store(e.into()))?;
        tx.put_challenges(id, &challenges).await.map_err(|e| store(e.into()))?;
        tx.commit().await.map_err(|e| store(e.into()))?;
        let timer = self.timer.clone().ok_or_else(|| {
            AcmeError::Unavailable("the control plane cannot wait for its nodes".into())
        })?;
        let mut waited = Duration::ZERO;
        loop {
            let mut held = true;
            for &node in &nodes {
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

    /// Keeps what was issued for `c`: read and checked as an upload is, its key sealed.
    async fn issued(&self, c: Certificate, issued: &Issued) -> Result<(), AppError> {
        let tls = self.tls()?;
        let checked = tls.read(&issued.chain, &issued.key).map_err(|e| e.to_string()).and_then(
            |(leaf, chain)| {
                leaf.check(self.clock.now()).map(|()| (leaf, chain)).map_err(|e| e.to_string())
            },
        );
        let (leaf, chain) = match checked {
            Ok(read) => read,
            Err(why) => return self.attempt_failed(c, &AcmeError::Refused(why)).await,
        };
        let sealed = tls.seal(c.id, &issued.key).map_err(|e| AppError::Internal(e.to_string()))?;
        let done = Certificate {
            names: leaf.names,
            chain,
            not_before: Some(leaf.not_before),
            not_after: Some(leaf.not_after),
            state: State::Issued,
            reason: None,
            renew_after: Some(renew_after(leaf.not_before, leaf.not_after)),
            revision: c.revision + 1,
            attempts: 0,
            next_attempt: None,
            ..c
        };
        let mut tx = self.store.begin().await?;
        tx.replace_certificate(&done, &sealed).await?;
        tx.append_effect(&effect(&done, Outcome::Succeeded, None)).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records a failed attempt: an issued certificate keeps serving, one never issued is
    /// failed; either way the next attempt waits out the backoff.
    async fn attempt_failed(&self, c: Certificate, e: &AcmeError) -> Result<(), AppError> {
        let attempts = c.attempts.saturating_add(1);
        let failed = Certificate {
            state: if c.state == State::Issued { State::Issued } else { State::Failed },
            reason: Some(e.to_string()),
            attempts,
            next_attempt: Some(self.clock.now() + backoff(attempts)),
            ..c
        };
        self.record(&failed, Some(e)).await
    }

    async fn record(&self, c: &Certificate, e: Option<&AcmeError>) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.record_attempt(c).await?;
        if let Some(e) = e {
            let failed = effect(c, Outcome::Failed, Some(&e.to_string()));
            tx.append_effect(&failed).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
