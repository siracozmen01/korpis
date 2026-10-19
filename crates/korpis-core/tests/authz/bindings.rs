// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity bindings (§5 of docs/08-identity.md): what a binding may say, and where its provider
//! may vouch.

use crate::support::Tree;
use korpis_core::authz::binding::{
    BindingError, BindingOrigin, BindingState, BindingTerms, IdentityBinding, ProviderRef, SetName,
};
use korpis_core::authz::{Scope, TemplateRef};
use korpis_core::principal::Principal;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

fn provider(tree: &Tree) -> ProviderRef {
    ProviderRef { organization: tree.reseller.id, name: "discord".into() }
}

fn set(p: &ProviderRef) -> Principal {
    Principal::ExternalSet { provider: p.key(), realm: "456".into(), member_of: "moderator".into() }
}

fn binding(p: &ProviderRef, subject: Principal) -> IdentityBinding {
    IdentityBinding {
        id: Uuid::now_v7(),
        provider: p.clone(),
        subject,
        origin: BindingOrigin::Operator,
        terms: BindingTerms::default(),
        state: BindingState::Unverified,
        last_verified_at: None,
        version: 1,
        made_by: Principal::User(Uuid::now_v7()),
        made_at: SystemTime::now(),
        retired_at: None,
    }
}

#[test]
fn a_provider_key_names_its_organization_and_parses_back() {
    let tree = Tree::new();
    let p = provider(&tree);
    assert!(p.key().starts_with("org_") && p.key().ends_with("/discord"));
    assert_eq!(ProviderRef::parse(&p.key()), Some(p.clone()));
    assert_eq!(ProviderRef::of(&set(&p)), Some(p));
    assert_eq!(ProviderRef::parse("discord"), None, "a bare name says nothing of where");
    assert_eq!(ProviderRef::parse("org_x/discord"), None);
}

#[test]
fn a_provider_vouches_at_its_organization_and_beneath_it_only() {
    let tree = Tree::new();
    let p = provider(&tree);
    for at in [tree.reseller, tree.customer, tree.survival, tree.w1] {
        assert!(p.reaches(&Scope::Object(at), &tree), "{at:?} is beneath the reseller");
    }
    assert!(!p.reaches(&Scope::Object(tree.host), &tree), "not above where it is installed");
    let customer = ProviderRef { organization: tree.customer.id, name: "oidc".into() };
    assert!(!customer.reaches(&Scope::Object(tree.reseller), &tree));
}

#[test]
fn a_binding_says_only_what_its_kind_may() {
    let tree = Tree::new();
    let p = provider(&tree);
    assert_eq!(binding(&p, set(&p)).validate(), Ok(()));
    let elsewhere = ProviderRef { organization: tree.customer.id, name: "discord".into() };
    let wrong = binding(&p, set(&elsewhere)).validate();
    assert!(matches!(wrong, Err(BindingError::Subject(_))), "the subject is this provider's");
    let mut first = binding(&p, set(&p));
    first.origin = BindingOrigin::FirstUse;
    assert_eq!(first.validate(), Err(BindingError::SetAtFirstUse));
    let person = Principal::ExternalUser { provider: p.key(), user: "123".into() };
    let mut b = binding(&p, person);
    assert_eq!(b.validate(), Ok(()));
    b.terms.template = Some(TemplateRef { template: Uuid::now_v7(), version: 1 });
    assert_eq!(b.validate(), Err(BindingError::SetOnly("template")));
    b.terms.template = None;
    b.terms.allows_irreversible = true;
    assert_eq!(b.validate(), Err(BindingError::SetOnly("allows_irreversible")));
    let mut b = binding(&p, set(&p));
    b.terms.reverify_interval = Duration::from_secs(30);
    assert_eq!(b.validate(), Err(BindingError::Interval));
    b.terms.reverify_interval = Duration::from_secs(3600);
    b.terms.grace = Duration::from_secs(7200);
    assert_eq!(b.validate(), Err(BindingError::Grace));
    b.terms.grace = Duration::ZERO;
    b.terms.require_membership = vec![SetName { realm: "4:5".into(), member_of: "x".into() }];
    assert_eq!(b.validate(), Err(BindingError::Membership));
}
