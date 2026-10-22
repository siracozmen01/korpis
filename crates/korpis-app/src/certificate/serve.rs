// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which certificate an endpoint serves with, and whether it can: a provided certificate is
//! named by its identifier in `TlsSpec.secret`, and must be one of the endpoint's organization,
//! issued and within its validity, and cover every name the endpoint routes. This is checked at
//! declaration and again when the routes are claimed, the certificate locked against a change
//! until the claim commits.

use crate::App;
use crate::error::AppError;
use crate::ports::{CertificateWrite as _, Clock, Exposure, RecipeSource, Store, Tx as _};
use korpis_core::dns::certificate::Certificate;
use korpis_core::dns::domain::DomainName;
use korpis_core::id::{Id, IdKind};
use korpis_core::workload::spec::endpoints::{EndpointSpec, TlsSpec};
use std::time::SystemTime;
use uuid::Uuid;

/// The certificate `e` names, if it names one; `at` is the endpoint's place in the body.
pub(crate) fn named(at: &str, e: &EndpointSpec) -> Result<Option<Uuid>, AppError> {
    match &e.tls {
        Some(TlsSpec::Provided { secret, .. }) => {
            Id::parse(IdKind::Certificate, secret).map(|id| Some(id.uuid())).map_err(|_| {
                AppError::invalid(format!("{at}.tls.secret"), "a certificate identifier, crt_…")
            })
        }
        _ => Ok(None),
    }
}

/// Checks that `found`, the certificate `id` an endpoint of `organization` names, can serve
/// `names` at `now`.
pub(crate) fn servable(
    (id, found): (Uuid, Option<&Certificate>),
    organization: Uuid,
    names: &[DomainName],
    now: SystemTime,
) -> Result<(), AppError> {
    let shown = Id::from_uuid(IdKind::Certificate, id);
    let Some(c) = found.filter(|c| c.organization == organization) else {
        let why = format!("{shown} is not a certificate of the endpoint's organization");
        return Err(AppError::conflict("endpoint.certificate", why));
    };
    if !c.valid_at(now) {
        let why = format!("{shown} is not issued, or not valid now");
        return Err(AppError::conflict("endpoint.certificate", why));
    }
    if let Some(name) = c.uncovered(names) {
        let why = format!("{shown} does not cover {}", name.as_str());
        return Err(AppError::conflict("endpoint.certificate", why));
    }
    Ok(())
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The certificate `id` and its key, for `node` to serve: answered only if an endpoint served
    /// with it is placed on that node, and only once it holds a key (§3.2 of
    /// docs/07-networking.md). That the node was given it is recorded first, so removing the
    /// node as compromised knows what to revoke (§9 of docs/17-security.md).
    pub async fn certificate_for_node(
        &self,
        node: Uuid,
        id: Uuid,
    ) -> Result<(Certificate, zeroize::Zeroizing<String>), AppError> {
        let not_here = || {
            let why = "no endpoint placed on this node is served with it";
            AppError::conflict("certificate.not_here", why)
        };
        let placed = self.store.placed_on(node).await?;
        let routed = self.store.routed_with(id).await?;
        if !routed.iter().any(|w| placed.contains(w)) {
            return Err(not_here());
        }
        let c = self.store.certificate(id).await?.ok_or_else(not_here)?;
        let sealed = self
            .store
            .sealed_key(id)
            .await?
            .ok_or_else(|| AppError::conflict("certificate.pending", "it holds no key yet"))?;
        let key = self.tls()?.open(id, &sealed).map_err(|e| AppError::Internal(e.to_string()))?;
        let exposure = Exposure {
            certificate: id,
            node,
            revision: c.revision,
            key_id: c.key_id.clone().unwrap_or_default(),
            chain: c.chain.clone(),
            at: self.clock.now(),
        };
        let mut tx = self.store.begin().await?;
        tx.record_exposure(&exposure).await?;
        tx.commit().await?;
        Ok((c, key))
    }
}
