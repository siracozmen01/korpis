// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity bindings (§5 of docs/08-identity.md).

use crate::{duration, duration_from, id, invalid, principal, timestamp, wire_id};
use korpis_core::authz::TemplateRef;
use korpis_core::authz::binding::{
    BindingOrigin, BindingState, BindingTerms, DEFAULT_GRACE, DEFAULT_REVERIFY, IdentityBinding,
    SetName,
};
use korpis_core::id::IdKind;
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;

/// A binding's terms from a request; the defaults of §5.1 where a duration is not said.
pub fn terms_from_pb(
    t: Option<&pb::IdentityBindingTerms>,
) -> Result<BindingTerms, crate::WireError> {
    let Some(t) = t else { return Ok(BindingTerms::default()) };
    let template = t
        .template
        .as_option()
        .map(|r| {
            Ok::<_, crate::WireError>(TemplateRef {
                template: id(IdKind::Template, "terms.template.template", &r.template)?,
                version: r.version,
            })
        })
        .transpose()?;
    let c = t.constraints.as_option();
    let grace = c
        .and_then(|c| c.grace.as_option())
        .map(|g| duration_from("terms.constraints.grace", g))
        .transpose()?
        .unwrap_or(DEFAULT_GRACE);
    let reverify_interval = t
        .reverify_interval
        .as_option()
        .map(|d| duration_from("terms.reverify_interval", d))
        .transpose()?
        .unwrap_or(DEFAULT_REVERIFY);
    Ok(BindingTerms {
        template,
        require_membership: c
            .map_or(&[][..], |c| &c.require_membership[..])
            .iter()
            .map(|s| SetName { realm: s.realm.clone(), member_of: s.member_of.clone() })
            .collect(),
        grace,
        reverify_interval,
        allows_irreversible: t.allows_irreversible,
    })
}

/// The external identity a request binds.
pub fn subject_from_pb(
    field: &str,
    e: Option<&pb::ExternalIdentity>,
) -> Result<Principal, crate::WireError> {
    let e = e.ok_or_else(|| invalid(field, "an external identity"))?;
    let subject = pb::Subject {
        kind: Some(pb::__buffa::oneof::subject::Kind::External(Box::new(e.clone()))),
        ..Default::default()
    };
    principal::from_pb(field, &subject)
}

fn terms_to_pb(t: &BindingTerms) -> pb::BindingConstraints {
    pb::BindingConstraints {
        require_membership: t
            .require_membership
            .iter()
            .map(|s| pb::ExternalSet {
                realm: s.realm.clone(),
                member_of: s.member_of.clone(),
                ..Default::default()
            })
            .collect(),
        grace: Some(duration(t.grace)).into(),
        ..Default::default()
    }
}

/// The wire form of a binding.
#[must_use]
pub fn binding_to_pb(b: &IdentityBinding) -> pb::IdentityBinding {
    let external = match principal::to_pb(&b.subject).kind {
        Some(pb::__buffa::oneof::subject::Kind::External(e)) => Some(*e),
        _ => None,
    };
    pb::IdentityBinding {
        id: wire_id(IdKind::Binding, b.id),
        provider: b.provider.name.clone(),
        subject: external.into(),
        kind: if b.is_set() {
            pb::BindingKind::BINDING_KIND_SET
        } else {
            pb::BindingKind::BINDING_KIND_INDIVIDUAL
        }
        .into(),
        created_by: match b.origin {
            BindingOrigin::Operator => pb::BindingOrigin::BINDING_ORIGIN_OPERATOR,
            BindingOrigin::FirstUse => pb::BindingOrigin::BINDING_ORIGIN_FIRST_USE,
        }
        .into(),
        template: b
            .terms
            .template
            .map(|t| pb::TemplateRef {
                template: wire_id(IdKind::Template, t.template),
                version: t.version,
                ..Default::default()
            })
            .into(),
        constraints: Some(terms_to_pb(&b.terms)).into(),
        reverify_interval: Some(duration(b.terms.reverify_interval)).into(),
        state: match b.state {
            BindingState::Verified => pb::BindingState::BINDING_STATE_VERIFIED,
            BindingState::Unverified => pb::BindingState::BINDING_STATE_UNVERIFIED,
        }
        .into(),
        last_verified_at: b.last_verified_at.map(timestamp).into(),
        organization: wire_id(IdKind::Organization, b.provider.organization),
        allows_irreversible: b.terms.allows_irreversible,
        version: b.version,
        made_by: Some(principal::to_pb(&b.made_by)).into(),
        made_at: Some(timestamp(b.made_at)).into(),
        retired_at: b.retired_at.map(timestamp).into(),
        ..Default::default()
    }
}
