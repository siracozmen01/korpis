// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::event::EventFilter;
use crate::extension::{ActionArgument, ArgumentType, NeedScope, SlotDecl};

fn action(a: &str) -> Action {
    Action::parse(a).unwrap()
}

fn dns() -> ExtensionManifest {
    ExtensionManifest {
        api_version: "korpis.extension.v1".into(),
        requires: vec!["provider.dns".into()],
        needs: vec![
            Need {
                actions: vec![action("endpoint.read")],
                scope: NeedScope::Organization,
                reason: "to publish every endpoint".into(),
            },
            Need {
                actions: vec![action("event.read")],
                scope: NeedScope::Organization,
                reason: "to hear of endpoint changes".into(),
            },
        ],
        egress: vec![Egress { host: "api.cloudflare.com".into(), port: 443 }],
        subscribes: vec![EventFilter { kinds: vec!["endpoint.".into()], ..EventFilter::default() }],
        provides: vec![ProvidedInterface { kind: ProviderType::Dns, name: "cloudflare".into() }],
        actions: vec![ActionDecl {
            name: "purge".into(),
            description: "Purges the zone's cache".into(),
            arguments: vec![ActionArgument {
                name: "workload".into(),
                kind: ArgumentType::Workload,
                required: true,
            }],
        }],
        ui_slots: vec![SlotDecl { slot: "workload.tab".into(), descriptor: "{}".into() }],
    }
}

fn refused(f: impl FnOnce(&mut ExtensionManifest)) -> String {
    let mut m = dns();
    f(&mut m);
    m.validate("cloudflare-dns").expect_err("refused").field
}

#[test]
fn a_well_formed_manifest_passes_and_each_rule_refuses_its_breach() {
    assert_eq!(dns().validate("cloudflare-dns"), Ok(()));
    assert_eq!(dns().validate("Cloudflare").unwrap_err().field, "metadata.name");
    assert_eq!(refused(|m| m.api_version = "v1".into()), "extension.api_version");
    assert_eq!(refused(|m| m.api_version = "korpis.extension.v".into()), "extension.api_version");
    assert_eq!(refused(|m| m.requires.push("provider.dns".into())), "extension.requires[1]");
    assert_eq!(refused(|m| m.needs[0].actions.clear()), "extension.needs[0].actions");
    assert_eq!(refused(|m| m.needs[0].reason = " ".into()), "extension.needs[0].reason");
    let twice = |m: &mut ExtensionManifest| m.needs[1].actions.push(action("endpoint.read"));
    assert_eq!(refused(twice), "extension.needs[1].actions", "one action, one scope, once");
    let mut elsewhere = dns();
    elsewhere.needs[1].actions.push(action("endpoint.read"));
    elsewhere.needs[1].scope = NeedScope::Target;
    assert_eq!(elsewhere.validate("cloudflare-dns"), Ok(()), "over another scope it is new");
    for bad in ["API.example.com", "-a.example", "", "exa mple.com"] {
        let host = |m: &mut ExtensionManifest| m.egress[0].host = bad.into();
        assert_eq!(refused(host), "extension.egress[0]", "{bad:?}");
    }
    assert_eq!(refused(|m| m.egress[0].port = 0), "extension.egress[0]");
    let mut v6 = dns();
    v6.egress[0].host = "2001:db8::1".into();
    assert_eq!(v6.validate("cloudflare-dns"), Ok(()), "an address is a host");
}

#[test]
fn subscriptions_providers_actions_and_slots_say_what_they_are_once() {
    assert_eq!(refused(|m| m.needs.truncate(1)), "extension.subscribes", "no event.read");
    let kind = |m: &mut ExtensionManifest| m.subscribes[0].kinds = vec!["Endpoint".into()];
    assert_eq!(refused(kind), "extension.subscribes[0].kinds");
    let project = |m: &mut ExtensionManifest| m.subscribes[0].projects = vec![uuid::Uuid::nil()];
    assert_eq!(refused(project), "extension.subscribes[0]", "an installation picks projects");
    let again = |m: &mut ExtensionManifest| m.provides.push(m.provides[0].clone());
    assert_eq!(refused(again), "extension.provides[1].name");
    let step = |m: &mut ExtensionManifest| {
        m.provides.push(ProvidedInterface { kind: ProviderType::Step, name: "steam".into() });
    };
    assert_eq!(refused(step), "extension.provides[1].name", "a step kind is <publisher>/<step>");
    let named = |m: &mut ExtensionManifest| m.actions[0].name = "Purge".into();
    assert_eq!(refused(named), "extension.actions[0].name");
    let argument = |m: &mut ExtensionManifest| {
        let a = m.actions[0].arguments[0].clone();
        m.actions[0].arguments.push(a);
    };
    assert_eq!(refused(argument), "extension.actions[0].arguments[1].name");
    let slot = |m: &mut ExtensionManifest| m.ui_slots.push(m.ui_slots[0].clone());
    assert_eq!(refused(slot), "extension.ui_slots[1].slot");
    assert!(ArgumentType::Workload.is_resource() && !ArgumentType::Int.is_resource());
    assert_eq!(ProviderType::parse("backup_target"), Some(ProviderType::BackupTarget));
    assert_eq!(NeedScope::parse("self"), Some(NeedScope::OwnWorkload));
}
