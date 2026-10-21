// SPDX-License-Identifier: AGPL-3.0-or-later

//! Changing a portal: serving it with another certificate, keeping everything else, and deleting
//! it. An `acme` certificate no portal names any more is removed by the issuer's sweep, as one no
//! endpoint routes is.

use super::{PortalView, effect, servable};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    CertificateWrite as _, Clock, EffectWrite as _, PortalWrite as _, RecipeSource, Store,
};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::dns::certificate::Source;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Serves a portal with `certificate`, a provided one of its organization, or with its own
    /// `acme` one again when it is `None`.
    pub async fn set_portal_certificate(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, PortalView>,
        (id, certificate): (Uuid, Option<Uuid>),
    ) -> Result<Reply<PortalView>, AppError> {
        const OPERATION: &str = "portal.certificate";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit =
            self.authorize(caller, "portal.write", ObjectRef::new(Kind::Portal, id)).await?;
        if let Some(c) = certificate {
            self.authorize(caller, "certificate.read", ObjectRef::new(Kind::Certificate, c))
                .await?;
        }
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let held = tx.lock_portal(id).await?.ok_or(AppError::NotFound { kind: "portal" })?;
        let current = tx.lock_certificate(held.certificate, false).await?;
        let next = match certificate {
            Some(c) => {
                let found = tx.lock_certificate(c, false).await?;
                servable((c, found.as_ref()), held.organization, &held.name, now)?;
                c
            }
            None if current
                .as_ref()
                .is_some_and(|c| c.source == Source::Acme && c.portal == Some(held.id)) =>
            {
                held.certificate
            }
            None => tx.acme_portal_certificate(held.organization, id, &held.name, now).await?,
        };
        tx.set_portal_certificate(id, next).await?;
        let portal = korpis_core::dns::portal::Portal { certificate: next, ..held };
        tx.append_effect(&effect(caller, OPERATION, permit.grant, &portal)).await?;
        let certificate = tx.lock_certificate(next, false).await?;
        finish(tx, caller, idem, OPERATION, PortalView { portal, certificate }).await
    }

    /// Deletes a portal.
    pub async fn delete_portal(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, ()>,
        id: Uuid,
    ) -> Result<Reply<()>, AppError> {
        const OPERATION: &str = "portal.delete";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit =
            self.authorize(caller, "portal.write", ObjectRef::new(Kind::Portal, id)).await?;
        let mut tx = self.store.begin().await?;
        let held = tx.lock_portal(id).await?.ok_or(AppError::NotFound { kind: "portal" })?;
        tx.delete_portal(id).await?;
        tx.append_effect(&effect(caller, OPERATION, permit.grant, &held)).await?;
        finish(tx, caller, idem, OPERATION, ()).await
    }
}
