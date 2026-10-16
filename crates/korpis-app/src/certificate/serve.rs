// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which certificate an endpoint serves with, and whether it can: a provided certificate is
//! named by its identifier in `TlsSpec.secret`, and must be one of the endpoint's organization,
//! issued and within its validity, and cover every name the endpoint routes. This is checked at
//! declaration and again when the routes are claimed, the certificate locked against a change
//! until the claim commits.

use crate::error::AppError;
use korpis_core::dns::certificate::Certificate;
use korpis_core::dns::domain::DomainName;
use korpis_core::id::{Id, IdKind};
use korpis_core::workload::spec::endpoints::{EndpointSpec, TlsSpec};
use std::time::SystemTime;
use uuid::Uuid;

/// The certificate `e` names, if it names one; `at` is the endpoint's place in the body.
pub(crate) fn named(at: &str, e: &EndpointSpec) -> Result<Option<Uuid>, AppError> {
    match &e.tls {
        Some(TlsSpec::Provided { secret }) => {
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
