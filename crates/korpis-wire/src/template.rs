// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grant templates (§4 of docs/08-identity.md).

use crate::authz::{actions, delegation_to_pb};
use crate::scope::{object_kind_from_pb, object_kind_to_pb, selector_from_pb, selector_to_pb};
use crate::{duration, duration_from, invalid, name, principal, timestamp, wire_id};
use korpis_core::authz::Delegation;
use korpis_core::authz::template::{
    GrantTemplate, ScopePattern, TemplateBody, TemplateConditions, TemplateVersion,
};
use korpis_core::id::IdKind;
use korpis_core::tenancy::Name;
use korpis_proto::korpis::v1 as pb;

/// A template's name and body from a request: every field said, the delegation policy above all.
pub fn body_from_pb(b: &pb::GrantTemplateBody) -> Result<(Name, TemplateBody), crate::WireError> {
    let pattern = b.scope_pattern.as_option();
    let kind = match pattern.map(|p| p.kind) {
        None => None,
        Some(k) if k.as_known() == Some(pb::ObjectKind::OBJECT_KIND_UNSPECIFIED) => None,
        Some(k) => Some(object_kind_from_pb("scope_pattern.kind", k)?),
    };
    let selector = pattern
        .and_then(|p| p.selector.as_option())
        .map(|s| selector_from_pb("scope_pattern.selector", Some(s)))
        .transpose()?;
    let d = b.delegation.as_option().ok_or_else(|| {
        invalid("template.delegation", "a template says whether its grants may delegate")
    })?;
    let delegation = Delegation {
        allowed: d.allowed,
        max_depth: d.max_depth,
        actions: actions("template.delegation.actions", &d.actions)?,
    };
    let c = b.conditions.as_option();
    let source_cidr = c
        .map_or(&[][..], |c| &c.source_cidr[..])
        .iter()
        .map(|n| n.parse().map_err(|_| invalid("conditions.source_cidr", format!("{n:?}"))))
        .collect::<Result<_, _>>()?;
    let conditions = TemplateConditions {
        lifetime: c
            .and_then(|c| c.lifetime.as_option())
            .map(|d| duration_from("conditions.lifetime", d))
            .transpose()?,
        source_cidr,
        requires_mfa: c.is_some_and(|c| c.requires_mfa),
        max_uses: c.and_then(|c| c.max_uses),
    };
    let body = TemplateBody {
        actions: actions("template.actions", &b.actions)?,
        pattern: ScopePattern { kind, selector },
        conditions,
        delegation,
    };
    Ok((name("template.name", &b.name)?, body))
}

/// The wire form of a template's pattern.
#[must_use]
pub fn pattern_to_pb(p: &ScopePattern) -> pb::ScopePattern {
    pb::ScopePattern {
        kind: p.kind.map_or(pb::ObjectKind::OBJECT_KIND_UNSPECIFIED, object_kind_to_pb).into(),
        selector: p.selector.as_ref().map(selector_to_pb).into(),
        ..Default::default()
    }
}

/// The wire form of a template's default conditions.
#[must_use]
pub fn conditions_to_pb(c: &TemplateConditions) -> pb::TemplateConditions {
    pb::TemplateConditions {
        lifetime: c.lifetime.map(duration).into(),
        source_cidr: c.source_cidr.iter().map(ToString::to_string).collect(),
        requires_mfa: c.requires_mfa,
        max_uses: c.max_uses,
        ..Default::default()
    }
}

/// The wire form of a template at the version it holds, its current one unless a past one was
/// asked for.
#[must_use]
pub fn template_to_pb(t: &GrantTemplate) -> pb::GrantTemplate {
    let v: &TemplateVersion = &t.current;
    pb::GrantTemplate {
        id: wire_id(IdKind::Template, t.id),
        organization: wire_id(IdKind::Organization, t.organization),
        name: v.name.as_str().to_owned(),
        version: v.version,
        actions: v.body.actions.iter().map(|a| a.as_str().to_owned()).collect(),
        scope_pattern: Some(pattern_to_pb(&v.body.pattern)).into(),
        delegation: Some(delegation_to_pb(&v.body.delegation)).into(),
        conditions: Some(conditions_to_pb(&v.body.conditions)).into(),
        updated_by: Some(principal::to_pb(&v.by)).into(),
        updated_at: Some(timestamp(v.at)).into(),
        retired_at: t.retired_at.map(timestamp).into(),
        ..Default::default()
    }
}
