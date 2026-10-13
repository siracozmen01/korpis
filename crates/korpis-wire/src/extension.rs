// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an extension's document declares, `korpis.v1.ExtensionManifest`, and the event filters
//! it subscribes with, `korpis.v1.EventFilter` (§3.1 of docs/16-extensions.md).

use crate::scope::selector_to_pb;
use crate::{IdKind, Result, invalid, wire_id};
use korpis_core::authz::Action;
use korpis_core::event::{EventFilter, Severity};
use korpis_core::extension::{
    ActionDecl, Egress, ExtensionManifest, Need, NeedScope, ProvidedInterface, ProviderType,
    SlotDecl,
};
use korpis_proto::korpis::v1 as pb;

enum_map!(need_scope_to_pb, need_scope_from_pb, NeedScope, pb::NeedScope, "a need scope", {
    NeedScope::Target => pb::NeedScope::NEED_SCOPE_TARGET,
    NeedScope::Organization => pb::NeedScope::NEED_SCOPE_ORGANIZATION,
    NeedScope::OwnWorkload => pb::NeedScope::NEED_SCOPE_SELF,
});

enum_map!(provider_type_to_pb, provider_type_from_pb, ProviderType, pb::ProviderType,
    "a provider type", {
    ProviderType::Dns => pb::ProviderType::PROVIDER_TYPE_DNS,
    ProviderType::BackupTarget => pb::ProviderType::PROVIDER_TYPE_BACKUP_TARGET,
    ProviderType::Step => pb::ProviderType::PROVIDER_TYPE_STEP,
    ProviderType::Health => pb::ProviderType::PROVIDER_TYPE_HEALTH,
    ProviderType::Console => pb::ProviderType::PROVIDER_TYPE_CONSOLE,
    ProviderType::Identity => pb::ProviderType::PROVIDER_TYPE_IDENTITY,
    ProviderType::EdgeRouter => pb::ProviderType::PROVIDER_TYPE_EDGE_ROUTER,
    ProviderType::ChatAdapter => pb::ProviderType::PROVIDER_TYPE_CHAT_ADAPTER,
});

enum_map!(severity_to_pb, severity_from_pb, Severity, pb::Severity, "a severity", {
    Severity::Info => pb::Severity::SEVERITY_INFO,
    Severity::Warning => pb::Severity::SEVERITY_WARNING,
    Severity::Critical => pb::Severity::SEVERITY_CRITICAL,
});

/// The wire form of a need.
#[must_use]
pub fn need_to_pb(n: &Need) -> pb::Need {
    pb::Need {
        actions: n.actions.iter().map(|a| a.as_str().to_owned()).collect(),
        scope: need_scope_to_pb(n.scope).into(),
        reason: n.reason.clone(),
        ..Default::default()
    }
}

/// A need from the field `field`, as an installer approves it.
pub fn need_from_pb(field: &str, n: &pb::Need) -> Result<Need> {
    let actions = n
        .actions
        .iter()
        .enumerate()
        .map(|(i, a)| {
            Action::parse(a).map_err(|e| invalid(&format!("{field}.actions[{i}]"), e.to_string()))
        })
        .collect::<Result<_>>()?;
    let scope = need_scope_from_pb(&format!("{field}.scope"), n.scope)?;
    Ok(Need { actions, scope, reason: n.reason.clone() })
}

/// `host:port`, an IPv6 host in brackets, as a document writes it.
fn egress_to_pb(e: &Egress) -> String {
    if e.host.contains(':') {
        format!("[{}]:{}", e.host, e.port)
    } else {
        format!("{}:{}", e.host, e.port)
    }
}

/// The wire form of an event filter.
#[must_use]
pub fn filter_to_pb(f: &EventFilter) -> pb::EventFilter {
    pb::EventFilter {
        kinds: f.kinds.clone(),
        min_severity: severity_to_pb(f.min_severity).into(),
        projects: f.projects.iter().map(|p| wire_id(IdKind::Project, *p)).collect(),
        workloads: selector_to_pb(&f.workloads).into(),
        ..Default::default()
    }
}

/// The wire form of a provided interface.
#[must_use]
pub fn provided_to_pb(p: &ProvidedInterface) -> pb::ProvidedInterface {
    pb::ProvidedInterface {
        r#type: provider_type_to_pb(p.kind).into(),
        name: p.name.clone(),
        ..Default::default()
    }
}

fn action_to_pb(a: &ActionDecl) -> pb::ActionDecl {
    pb::ActionDecl {
        name: a.name.clone(),
        description: a.description.clone(),
        arguments: a
            .arguments
            .iter()
            .map(|x| pb::ActionArgument {
                name: x.name.clone(),
                r#type: x.kind.as_str().to_owned(),
                resource: x.kind.is_resource(),
                required: x.required,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn slot_to_pb(s: &SlotDecl) -> pb::SlotDecl {
    pb::SlotDecl { slot: s.slot.clone(), descriptor: s.descriptor.clone(), ..Default::default() }
}

/// The wire form of a manifest.
#[must_use]
pub fn manifest_to_pb(m: &ExtensionManifest) -> pb::ExtensionManifest {
    pb::ExtensionManifest {
        api_version: m.api_version.clone(),
        requires: m.requires.clone(),
        needs: m.needs.iter().map(need_to_pb).collect(),
        egress: m.egress.iter().map(egress_to_pb).collect(),
        subscribes: m.subscribes.iter().map(filter_to_pb).collect(),
        provides: m.provides.iter().map(provided_to_pb).collect(),
        actions: m.actions.iter().map(action_to_pb).collect(),
        ui_slots: m.ui_slots.iter().map(slot_to_pb).collect(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_need_crosses_the_wire_and_back_and_a_bad_one_names_its_field() {
        let need = Need {
            actions: vec![Action::parse("event.read").unwrap_or_else(|e| panic!("{e}"))],
            scope: NeedScope::OwnWorkload,
            reason: "to hear of changes".into(),
        };
        assert_eq!(need_from_pb("n", &need_to_pb(&need)), Ok(need));
        let bad = pb::Need { actions: vec!["event.own".into()], ..Default::default() };
        assert_eq!(need_from_pb("n", &bad).map_err(|e| e.field), Err("n.actions[0]".into()));
        let v6 = Egress { host: "2001:db8::1".into(), port: 443 };
        assert_eq!(egress_to_pb(&v6), "[2001:db8::1]:443");
    }
}
