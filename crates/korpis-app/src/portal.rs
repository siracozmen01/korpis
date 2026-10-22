// SPDX-License-Identifier: AGPL-3.0-or-later

//! Portals (§3.6 of docs/07-networking.md): names the control plane answers under for an
//! organization. A portal is made under `portal.write` on its organization, of a name the
//! organization holds, that no endpoint routes and no other portal has, and that is not one of the
//! installation's own hosts. All of it is checked under the domains lock, which a route's claim
//! and every change to which domains are verified take too, so no two of them can each find the
//! name free. A portal is served with its own `acme` certificate, made with it, or with a
//! `provided` one of its organization covering its name, named under `certificate.read` on it.

mod change;
mod serve;

pub use serve::Served;

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged, visible};
use crate::ports::{
    CertificateWrite as _, Clock, DomainWrite as _, EffectWrite as _, PortalWrite as _,
    RecipeSource, Store, StoreError, Unique,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::dns::certificate::Certificate;
use korpis_core::dns::domain::{DomainName, holder_of};
use korpis_core::dns::portal::{Portal, installations_own};
use korpis_core::id::{Id, IdKind};
use std::time::SystemTime;
use uuid::Uuid;

/// A portal, with the certificate it is served with as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalView {
    /// The portal.
    pub portal: Portal,
    /// Its certificate, which a portal always names, read beside it.
    pub certificate: Option<Certificate>,
}

/// What deleting a portal would remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalReach {
    /// The passkeys registered at it.
    pub passkeys: u64,
    /// The accounts with a passkey there and nothing else that signs in.
    pub accounts_only_here: u64,
}

/// A portal to make.
#[derive(Debug, Clone)]
pub struct CreatePortal {
    /// The organization whose it will be.
    pub organization: Uuid,
    /// The name.
    pub name: DomainName,
    /// A provided certificate of the organization to serve it with; its own `acme` one if none.
    pub certificate: Option<Uuid>,
}

/// The host of `origin`, `https://host[:port]`.
fn host_of(origin: &str) -> Option<&str> {
    let rest = origin.split_once("://").map_or(origin, |(_, r)| r);
    let host = rest
        .rsplit_once(':')
        .map_or(rest, |(h, port)| if port.bytes().all(|b| b.is_ascii_digit()) { h } else { rest });
    (!host.is_empty()).then_some(host)
}

/// Checks that `found`, the certificate `id` named for a portal of `organization`, can serve
/// `name` at `now`: the organization's, issued and valid, and covering it.
pub(crate) fn servable(
    (id, found): (Uuid, Option<&Certificate>),
    organization: Uuid,
    name: &DomainName,
    now: SystemTime,
) -> Result<(), AppError> {
    let shown = Id::from_uuid(IdKind::Certificate, id);
    let why = match found.filter(|c| c.organization == organization) {
        None => format!("{shown} is not a certificate of the portal's organization"),
        Some(c) if !c.valid_at(now) => format!("{shown} is not issued, or not valid now"),
        Some(c) if c.uncovered(std::slice::from_ref(name)).is_some() => {
            format!("{shown} does not cover {}", name.as_str())
        }
        Some(_) => return Ok(()),
    };
    Err(AppError::conflict("portal.certificate", why))
}

pub(crate) fn effect(caller: &Caller, action: &str, grant: Uuid, p: &Portal) -> Effect {
    // About the organization, not the portal, so a deletion's effect keeps its home.
    let about = ObjectRef::new(Kind::Organization, p.organization);
    caller.effect(action, Outcome::Succeeded).under(grant).about(about).after(details([
        ("portal", p.id.to_string().into()),
        ("name", p.name.as_str().into()),
        ("certificate", p.certificate.to_string().into()),
    ]))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The installation's own hosts, which no portal may be: its public URL's and each
    /// `--web-origin`'s, which the relying party's origins are.
    fn installation_hosts(&self) -> Vec<String> {
        self.relying_party.as_ref().map_or_else(Vec::new, |rp| {
            rp.origins.iter().filter_map(|o| host_of(o)).map(str::to_owned).collect()
        })
    }

    /// The port the installation is reached on, from its public URL's origin; none for HTTPS's
    /// own, or without a public URL, so a portal's origin is `https://<name>` (§3.6 of
    /// docs/07-networking.md).
    #[must_use]
    pub fn public_port(&self) -> Option<u16> {
        let own = self.relying_party.as_ref()?.origins.first()?;
        let port = own.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok())?;
        (port != 443).then_some(port)
    }

    /// The organization of `portal`, none if it is gone: what its sign-in page wears and offers.
    pub(crate) async fn portal_organization(&self, portal: Uuid) -> Result<Option<Uuid>, AppError> {
        Ok(self.store.portal(portal).await?.map(|p| p.organization))
    }

    /// The origin of every portal, whose panels open sessions at the relays (§3.6 of
    /// docs/07-networking.md).
    pub async fn portal_origins(&self) -> Result<Vec<String>, AppError> {
        let port = self.public_port();
        Ok(self.store.all_portals().await?.iter().map(|p| p.origin(port)).collect())
    }

    /// Checks in `tx`, under the domains lock, that `name` may be a portal of `organization`:
    /// held by it, and routed by no endpoint.
    async fn portal_name_free(
        tx: &mut S::Tx,
        organization: Uuid,
        name: &DomainName,
    ) -> Result<(), AppError> {
        tx.lock_domains().await?;
        let covering = tx.verified_overlapping(name).await?;
        if holder_of(name, &covering) != Some(organization) {
            let why = format!(
                "{} is not the organization's: no verified domain of it is the most specific \
                 covering the name",
                name.as_str()
            );
            return Err(AppError::conflict("portal.name_not_held", why));
        }
        if tx.routes_beneath(name).await?.iter().any(|r| &r.name == name) {
            let why = format!("{} is routed by an endpoint", name.as_str());
            return Err(AppError::conflict("portal.name_routed", why));
        }
        Ok(())
    }

    /// Makes a portal.
    pub async fn create_portal(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, PortalView>,
        cmd: CreatePortal,
    ) -> Result<Reply<PortalView>, AppError> {
        const OPERATION: &str = "portal.create";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let org = ObjectRef::new(Kind::Organization, cmd.organization);
        let permit = self.authorize(caller, "portal.write", org).await?;
        if installations_own(&cmd.name, &self.installation_hosts()) {
            let why = "the installation's own address is served already";
            return Err(AppError::conflict("portal.installation", why));
        }
        if let Some(c) = cmd.certificate {
            self.authorize(caller, "certificate.read", ObjectRef::new(Kind::Certificate, c))
                .await?;
        }
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        Self::portal_name_free(&mut tx, cmd.organization, &cmd.name).await?;
        let id = Uuid::now_v7();
        let certificate = match cmd.certificate {
            Some(c) => {
                let found = tx.lock_certificate(c, false).await?;
                servable((c, found.as_ref()), cmd.organization, &cmd.name, now)?;
                c
            }
            None => tx.acme_portal_certificate(cmd.organization, id, &cmd.name, now).await?,
        };
        let portal = Portal {
            id,
            organization: cmd.organization,
            name: cmd.name,
            certificate,
            created_at: now,
        };
        tx.insert_portal(&portal).await.map_err(|e| match e {
            StoreError::Duplicate(Unique::PortalName) => {
                AppError::conflict("portal.taken", "another portal has this name")
            }
            other => other.into(),
        })?;
        tx.append_effect(&effect(caller, OPERATION, permit.grant, &portal)).await?;
        let certificate = tx.lock_certificate(certificate, false).await?;
        finish(tx, caller, idem, OPERATION, PortalView { portal, certificate }).await
    }

    /// One portal the caller may read, with its certificate.
    pub async fn get_portal(&self, caller: &Caller, id: Uuid) -> Result<PortalView, AppError> {
        self.authorize(caller, "portal.read", ObjectRef::new(Kind::Portal, id)).await?;
        let portal = self.store.portal(id).await?.ok_or(AppError::NotFound { kind: "portal" })?;
        self.viewed(portal).await
    }

    /// What deleting portal `id`, which the caller may read, would remove: the passkeys
    /// registered there, and the accounts that sign in nowhere else (§9.8 of
    /// docs/08-identity.md).
    pub async fn portal_reach(&self, caller: &Caller, id: Uuid) -> Result<PortalReach, AppError> {
        self.authorize(caller, "portal.read", ObjectRef::new(Kind::Portal, id)).await?;
        let (passkeys, accounts_only_here) = self.store.portal_reach(id).await?;
        Ok(PortalReach { passkeys, accounts_only_here })
    }

    /// `portal`, with its certificate read beside it.
    pub(crate) async fn viewed(&self, portal: Portal) -> Result<PortalView, AppError> {
        let certificate = self.store.certificate(portal.certificate).await?;
        Ok(PortalView { portal, certificate })
    }

    /// An organization's portals the caller may read.
    pub async fn list_portals(
        &self,
        caller: &Caller,
        organization: Uuid,
        page: Page,
    ) -> Result<Paged<PortalView>, AppError> {
        let read = |p: Page| async move {
            let found = self.store.portals_of(organization, p).await?;
            let mut viewed = Vec::with_capacity(found.len());
            for portal in found {
                viewed.push(self.viewed(portal).await?);
            }
            Ok(viewed)
        };
        let readable = |found: Vec<PortalView>| async move {
            let objects: Vec<ObjectRef> =
                found.iter().map(|v| ObjectRef::new(Kind::Portal, v.portal.id)).collect();
            let shown = self.permitted_each(caller, "portal.read", &objects).await?;
            Ok(found.into_iter().zip(shown).filter_map(|(v, s)| s.then_some(v)).collect())
        };
        visible(page, |v| v.portal.id, read, readable).await
    }
}

#[cfg(test)]
mod tests {
    use super::host_of;

    #[test]
    fn an_origin_s_host_is_what_lies_between_its_scheme_and_its_port() {
        assert_eq!(host_of("https://korpis.example.com"), Some("korpis.example.com"));
        assert_eq!(host_of("https://korpis.example.com:7443"), Some("korpis.example.com"));
        assert_eq!(host_of("http://localhost:7443"), Some("localhost"));
        assert_eq!(host_of("https://"), None);
    }
}
