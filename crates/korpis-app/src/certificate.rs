// SPDX-License-Identifier: AGPL-3.0-or-later

//! Certificates organizations hold for the ingress (§3.2 of docs/07-networking.md). A provided
//! certificate is uploaded under `certificate.write` on its organization, chain and key: the
//! control plane's TLS key reads the chain, shows the key to be its leaf's, and seals the key,
//! which is stored no other way. It may be replaced in place, keeping its identifier, by a chain
//! that still covers every name served with it, and deleted once no endpoint serves with it;
//! which endpoints do is recorded on their routes, so the check holds however a deletion and a
//! claim interleave.

mod issue;
mod serve;

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{CertificateWrite as _, Clock, EffectWrite as _, RecipeSource, Store, TlsVault};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::dns::certificate::{Certificate, Leaf, Source, State};
use korpis_core::dns::domain::DomainName;
use std::sync::Arc;
use uuid::Uuid;

pub(crate) use serve::{named, servable};

/// A chain and its key, in PEM, as uploaded.
#[derive(Clone)]
pub struct Upload {
    /// The chain, leaf first.
    pub chain: String,
    /// The leaf's private key.
    pub key: zeroize::Zeroizing<String>,
}

impl std::fmt::Debug for Upload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upload").field("chain", &self.chain).finish_non_exhaustive()
    }
}

fn effect(caller: &Caller, action: &str, grant: Uuid, c: &Certificate) -> Effect {
    Effect::new(caller.principal.clone(), action, Outcome::Succeeded).under(grant).after(details([
        ("certificate", c.id.to_string().into()),
        ("names", c.names.join(",").into()),
        ("revision", c.revision.into()),
    ]))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    pub(crate) fn tls(&self) -> Result<&Arc<dyn TlsVault>, AppError> {
        self.tls.as_ref().ok_or_else(|| AppError::Unavailable {
            what: "certificates",
            detail: "the control plane was started without its TLS key".into(),
        })
    }

    /// What `upload`'s leaf says, its chain as served, and its key sealed to `id`, refused unless
    /// the key is the leaf's and the leaf can be a certificate now.
    fn read_upload(
        &self,
        id: Uuid,
        upload: &Upload,
    ) -> Result<(Leaf, String, crate::ports::SealedKey), AppError> {
        let tls = self.tls()?;
        let (leaf, chain) = tls.read(&upload.chain, &upload.key).map_err(|e| match e {
            crate::ports::ReadError::Chain(why) => AppError::invalid("chain", why),
            crate::ports::ReadError::Key(why) => AppError::invalid("key", why),
        })?;
        leaf.check(self.clock.now()).map_err(|e| AppError::invalid("chain", e.to_string()))?;
        let sealed = tls.seal(id, &upload.key).map_err(|e| AppError::Internal(e.to_string()))?;
        Ok((leaf, chain, sealed))
    }

    /// Uploads a certificate for `organization`.
    pub async fn create_certificate(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Certificate>,
        (organization, upload): (Uuid, Upload),
    ) -> Result<Reply<Certificate>, AppError> {
        const OPERATION: &str = "certificate.create";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let org = ObjectRef::new(Kind::Organization, organization);
        let permit = self.authorize(caller, "certificate.write", org).await?;
        let id = Uuid::now_v7();
        let (leaf, chain, sealed) = self.read_upload(id, &upload)?;
        let created = Certificate {
            id,
            organization,
            names: leaf.names,
            source: Source::Provided,
            chain,
            not_before: Some(leaf.not_before),
            not_after: Some(leaf.not_after),
            state: State::Issued,
            reason: None,
            renew_after: None,
            revision: 1,
            created_at: self.clock.now(),
            owner: None,
            attempts: 0,
            next_attempt: None,
        };
        let mut tx = self.store.begin().await?;
        tx.insert_certificate(&created, Some(&sealed)).await?;
        tx.append_effect(&effect(caller, OPERATION, permit.grant, &created)).await?;
        finish(tx, caller, idem, OPERATION, created).await
    }

    /// Replaces a provided certificate's chain and key, keeping its identifier, so the endpoints
    /// serving with it serve the new one: the new leaf must cover every name they route.
    pub async fn replace_certificate(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Certificate>,
        (id, upload): (Uuid, Upload),
    ) -> Result<Reply<Certificate>, AppError> {
        const OPERATION: &str = "certificate.replace";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let (_, grant) = self.certificate_for(caller, "certificate.write", id).await?;
        let (leaf, chain, sealed) = self.read_upload(id, &upload)?;
        let mut tx = self.store.begin().await?;
        let held = tx.lock_certificate(id, true).await?;
        let held = held.ok_or(AppError::NotFound { kind: "certificate" })?;
        if held.source != Source::Provided {
            let why = "an acme certificate is the control plane's to issue and renew";
            return Err(AppError::conflict("certificate.acme", why));
        }
        let replaced = Certificate {
            names: leaf.names,
            chain,
            not_before: Some(leaf.not_before),
            not_after: Some(leaf.not_after),
            state: State::Issued,
            reason: None,
            revision: held.revision + 1,
            ..held
        };
        let served: Vec<DomainName> =
            tx.routes_served_by(id).await?.into_iter().map(|r| r.name).collect();
        if let Some(name) = replaced.uncovered(&served) {
            let why =
                format!("{} is served with it, and the new chain does not cover it", name.as_str());
            return Err(AppError::conflict("certificate.uncovered", why));
        }
        tx.replace_certificate(&replaced, &sealed).await?;
        tx.append_effect(&effect(caller, OPERATION, grant, &replaced)).await?;
        finish(tx, caller, idem, OPERATION, replaced).await
    }

    /// Deletes a certificate no endpoint serves with.
    pub async fn delete_certificate(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, ()>,
        id: Uuid,
    ) -> Result<Reply<()>, AppError> {
        const OPERATION: &str = "certificate.delete";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let (c, grant) = self.certificate_for(caller, "certificate.write", id).await?;
        let mut tx = self.store.begin().await?;
        if tx.lock_certificate(id, true).await?.is_none() {
            return Err(AppError::NotFound { kind: "certificate" });
        }
        if let Some(r) = tx.routes_served_by(id).await?.first() {
            let why = format!(
                "endpoint {} serves {} with it; change the endpoint first",
                r.endpoint,
                r.name.as_str()
            );
            return Err(AppError::conflict("certificate.in_use", why));
        }
        tx.delete_certificate(id).await?;
        tx.append_effect(&effect(caller, OPERATION, grant, &c)).await?;
        finish(tx, caller, idem, OPERATION, ()).await
    }

    /// A certificate, authorized for `action`.
    async fn certificate_for(
        &self,
        caller: &Caller,
        action: &str,
        id: Uuid,
    ) -> Result<(Certificate, Uuid), AppError> {
        let permit = self.authorize(caller, action, ObjectRef::new(Kind::Certificate, id)).await?;
        let c = self.store.certificate(id).await?;
        Ok((c.ok_or(AppError::NotFound { kind: "certificate" })?, permit.grant))
    }

    /// One certificate the caller may read, without its key.
    pub async fn get_certificate(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<Certificate, AppError> {
        Ok(self.certificate_for(caller, "certificate.read", id).await?.0)
    }

    /// An organization's certificates the caller may read, without their keys.
    pub async fn list_certificates(
        &self,
        caller: &Caller,
        organization: Uuid,
        page: Page,
    ) -> Result<Paged<Certificate>, AppError> {
        let found = self.store.certificates_of(organization, page).await?;
        let next = page.next(&found, |c| c.id);
        let objects: Vec<ObjectRef> =
            found.iter().map(|c| ObjectRef::new(Kind::Certificate, c.id)).collect();
        let readable = self.permitted_each(caller, "certificate.read", &objects).await?;
        let items = found.into_iter().zip(readable).filter_map(|(c, ok)| ok.then_some(c)).collect();
        Ok(Paged { items, next })
    }
}
