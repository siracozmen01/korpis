// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grants: conditions, delegation policies, and the grant itself.

use crate::invalid;
use crate::{principal, scope, time, timestamp, wire_id};

use korpis_core::authz::{Action, Conditions, Delegation, GrantRecord, GrantState};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;
use std::time::SystemTime;

/// Actions from the request field `field`.
pub fn actions(field: &str, names: &[String]) -> Result<BTreeSet<Action>, crate::WireError> {
    names.iter().map(|n| Action::parse(n).map_err(|e| invalid(field, e.to_string()))).collect()
}

/// Conditions from a request; absent means none.
pub fn conditions_from_pb(c: Option<&pb::Conditions>) -> Result<Conditions, crate::WireError> {
    let Some(c) = c else { return Ok(Conditions::default()) };
    let source_cidr = c
        .source_cidr
        .iter()
        .map(|n| n.parse().map_err(|_| invalid("conditions.source_cidr", format!("{n:?}"))))
        .collect::<Result<_, _>>()?;
    Ok(Conditions {
        expires_at: c
            .expires_at
            .as_option()
            .map(|t| time("conditions.expires_at", t))
            .transpose()?,
        not_before: c
            .not_before
            .as_option()
            .map(|t| time("conditions.not_before", t))
            .transpose()?,
        source_cidr,
        requires_mfa: c.requires_mfa,
        requires_approval_by: c
            .requires_approval_by
            .as_option()
            .map(|s| scope::from_pb("conditions.requires_approval_by", s))
            .transpose()?,
        max_uses: c.max_uses,
    })
}

/// The wire form of conditions.
#[must_use]
pub fn conditions_to_pb(c: &Conditions) -> pb::Conditions {
    pb::Conditions {
        expires_at: c.expires_at.map(timestamp).into(),
        not_before: c.not_before.map(timestamp).into(),
        source_cidr: c.source_cidr.iter().map(ToString::to_string).collect(),
        requires_mfa: c.requires_mfa,
        requires_approval_by: c.requires_approval_by.as_ref().map(scope::to_pb).into(),
        max_uses: c.max_uses,
        ..Default::default()
    }
}

/// A delegation policy from a request; absent means none.
pub fn delegation_from_pb(
    d: Option<&pb::DelegationPolicy>,
) -> Result<Delegation, crate::WireError> {
    let Some(d) = d else { return Ok(Delegation::default()) };
    Ok(Delegation {
        allowed: d.allowed,
        max_depth: d.max_depth,
        actions: actions("delegation.actions", &d.actions)?,
    })
}

/// The wire form of a delegation policy.
#[must_use]
pub fn delegation_to_pb(d: &Delegation) -> pb::DelegationPolicy {
    pb::DelegationPolicy {
        allowed: d.allowed,
        max_depth: d.max_depth,
        actions: d.actions.iter().map(|a| a.as_str().to_owned()).collect(),
        ..Default::default()
    }
}

fn state(s: GrantState) -> pb::GrantState {
    match s {
        GrantState::Issued => pb::GrantState::GRANT_STATE_ISSUED,
        GrantState::Expired => pb::GrantState::GRANT_STATE_EXPIRED,
        GrantState::Exhausted => pb::GrantState::GRANT_STATE_EXHAUSTED,
        GrantState::Revoked => pb::GrantState::GRANT_STATE_REVOKED,
        GrantState::Suspended => pb::GrantState::GRANT_STATE_SUSPENDED,
    }
}

/// The wire form of a grant, with its state at `now`.
#[must_use]
pub fn grant_to_pb(r: &GrantRecord, now: SystemTime) -> pb::Grant {
    let g = &r.grant;
    pb::Grant {
        id: wire_id(IdKind::Grant, g.id),
        subject: principal::to_pb(&r.holder).into(),
        actions: g.actions.iter().map(|a| a.as_str().to_owned()).collect(),
        scope: scope::to_pb(&g.scope).into(),
        conditions: conditions_to_pb(&g.conditions).into(),
        parent: g.parent.map(|p| wire_id(IdKind::Grant, p)),
        delegation: delegation_to_pb(&g.delegation).into(),
        issued_by: principal::to_pb(&r.issued_by).into(),
        issued_at: timestamp(r.issued_at).into(),
        revoked_at: r.revoked_at.map(timestamp).into(),
        link_form: g.link_form,
        state: state(r.state(now)).into(),
        label: r.label.clone(),
        ..Default::default()
    }
}
