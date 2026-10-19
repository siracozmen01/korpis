// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grant templates (§4 of docs/08-identity.md): what a template may ask for, and where it is
//! offered. A template says whether its grants delegate, never more than it permits and never
//! without a depth; a selector needs an organization or a project to look below; and a template
//! is offered to its owner's subtree while it is not retired.

use korpis_core::authz::template::{
    GrantTemplate, ScopePattern, TemplateBody, TemplateConditions, TemplateError, TemplateVersion,
};
use korpis_core::authz::{Action, Delegation, Kind, LabelSelector};
use korpis_core::principal::Principal;
use korpis_core::tenancy::Name;
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

fn actions(names: &[&str]) -> BTreeSet<Action> {
    names.iter().map(|n| Action::parse(n).unwrap()).collect()
}

/// The moderator of §3.2: restart, and pass restart on one level.
fn moderator() -> TemplateBody {
    TemplateBody {
        actions: actions(&["workload.restart", "workload.console.read"]),
        pattern: ScopePattern { kind: Some(Kind::Project), selector: None },
        conditions: TemplateConditions {
            lifetime: Some(Duration::from_secs(90 * 86_400)),
            ..TemplateConditions::default()
        },
        delegation: Delegation {
            allowed: true,
            max_depth: Some(1),
            actions: actions(&["workload.restart"]),
        },
    }
}

#[test]
fn a_moderator_template_is_valid_and_each_way_to_widen_it_is_named() {
    assert_eq!(moderator().validate(), Ok(()));
    let refused = |change: fn(&mut TemplateBody)| {
        let mut body = moderator();
        change(&mut body);
        body.validate().unwrap_err()
    };
    assert_eq!(refused(|b| b.actions.clear()), TemplateError::NoActions);
    assert_eq!(refused(|b| b.pattern.kind = Some(Kind::Volume)), TemplateError::Kind);
    assert_eq!(
        refused(|b| {
            b.pattern = ScopePattern { kind: None, selector: Some(LabelSelector::default()) };
        }),
        TemplateError::SelectorKind,
        "nothing below to select from"
    );
    assert_eq!(refused(|b| b.delegation.max_depth = None), TemplateError::Unbounded);
    assert_eq!(refused(|b| b.delegation.max_depth = Some(0)), TemplateError::Unbounded);
    assert_eq!(
        refused(|b| b.delegation.actions = actions(&["workload.exec"])),
        TemplateError::DelegatesMore
    );
    assert_eq!(
        refused(|b| b.delegation.allowed = false),
        TemplateError::Inert,
        "a depth on a template that does not delegate is a mistake said, not ignored"
    );
    assert_eq!(refused(|b| b.conditions.lifetime = Some(Duration::ZERO)), TemplateError::Lifetime);
    assert_eq!(refused(|b| b.conditions.max_uses = Some(0)), TemplateError::Uses);
    assert_eq!(TemplateError::Unbounded.field(), "delegation");

    let mut selected = moderator();
    selected.pattern.selector = Some(LabelSelector::default());
    assert_eq!(selected.validate(), Ok(()), "a selector below a project");
    let mut inert = moderator();
    inert.delegation = Delegation::default();
    assert_eq!(inert.validate(), Ok(()), "a template that does not delegate");
}

#[test]
fn a_template_is_offered_to_its_owners_subtree_until_it_is_retired() {
    let (host, reseller, customer) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let mut template = GrantTemplate {
        id: Uuid::now_v7(),
        organization: reseller,
        current: TemplateVersion {
            version: 1,
            name: Name::parse("Moderator").unwrap(),
            body: moderator(),
            by: Principal::User(Uuid::now_v7()),
            at: SystemTime::now(),
        },
        retired_at: None,
    };
    assert!(template.offered_within(&[customer, reseller, host]), "beneath its owner");
    assert!(template.offered_within(&[reseller, host]), "at its owner");
    assert!(!template.offered_within(&[host]), "never above it");
    template.retired_at = Some(SystemTime::now());
    assert!(!template.offered_within(&[customer, reseller, host]), "retired: nowhere");
}
