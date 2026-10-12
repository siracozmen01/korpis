// SPDX-License-Identifier: AGPL-3.0-or-later

//! Principals: `korpis.v1.Subject`.

use super::super::error::invalid;
use super::{id, wire_id};
use buffa::EnumValue;
use connectrpc::ConnectError;
use korpis_core::id::IdKind;
use korpis_core::principal::{Principal, SystemActor};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::{external_identity, subject};

fn system_actor(a: EnumValue<pb::SystemActor>) -> Option<SystemActor> {
    match a.as_known()? {
        pb::SystemActor::SYSTEM_ACTOR_RECONCILER => Some(SystemActor::Reconciler),
        pb::SystemActor::SYSTEM_ACTOR_SCHEDULER => Some(SystemActor::Scheduler),
        pb::SystemActor::SYSTEM_ACTOR_BOOTSTRAP => Some(SystemActor::Bootstrap),
        pb::SystemActor::SYSTEM_ACTOR_UNSPECIFIED => None,
    }
}

fn external(e: &pb::ExternalIdentity) -> Option<Principal> {
    let provider = e.provider.clone();
    match e.selector.as_ref()? {
        external_identity::Selector::User(user) => {
            Some(Principal::ExternalUser { provider, user: user.clone() })
        }
        external_identity::Selector::Set(set) => Some(Principal::ExternalSet {
            provider,
            realm: set.realm.clone(),
            member_of: set.member_of.clone(),
        }),
    }
}

/// The principal a request's subject names.
pub fn from_pb(field: &str, s: &pb::Subject) -> Result<Principal, ConnectError> {
    let none = || invalid(field, "a subject must name exactly one actor");
    let p = match s.kind.as_ref().ok_or_else(none)? {
        subject::Kind::User(v) => Principal::User(id(IdKind::User, field, v)?),
        subject::Kind::Token(v) => Principal::Token(id(IdKind::Token, field, v)?),
        subject::Kind::Workload(v) => Principal::Workload(id(IdKind::Workload, field, v)?),
        subject::Kind::Extension(name) => Principal::Extension(name.clone()),
        subject::Kind::System(a) => Principal::System(system_actor(*a).ok_or_else(none)?),
        subject::Kind::External(e) => external(e).ok_or_else(none)?,
    };
    p.validate().map_err(|e| invalid(field, e.to_string()))?;
    Ok(p)
}

/// The wire form of a principal.
#[must_use]
pub fn to_pb(p: &Principal) -> pb::Subject {
    let kind = match p {
        Principal::User(u) => subject::Kind::User(wire_id(IdKind::User, *u)),
        Principal::Token(t) => subject::Kind::Token(wire_id(IdKind::Token, *t)),
        Principal::Workload(w) => subject::Kind::Workload(wire_id(IdKind::Workload, *w)),
        Principal::Extension(name) => subject::Kind::Extension(name.clone()),
        Principal::System(a) => subject::Kind::System(
            match a {
                SystemActor::Reconciler => pb::SystemActor::SYSTEM_ACTOR_RECONCILER,
                SystemActor::Scheduler => pb::SystemActor::SYSTEM_ACTOR_SCHEDULER,
                SystemActor::Bootstrap => pb::SystemActor::SYSTEM_ACTOR_BOOTSTRAP,
            }
            .into(),
        ),
        Principal::ExternalUser { provider, user } => {
            subject::Kind::External(Box::new(pb::ExternalIdentity {
                provider: provider.clone(),
                selector: Some(external_identity::Selector::User(user.clone())),
                ..Default::default()
            }))
        }
        Principal::ExternalSet { provider, realm, member_of } => {
            subject::Kind::External(Box::new(pb::ExternalIdentity {
                provider: provider.clone(),
                selector: Some(external_identity::Selector::Set(Box::new(pb::ExternalSet {
                    realm: realm.clone(),
                    member_of: member_of.clone(),
                    ..Default::default()
                }))),
                ..Default::default()
            }))
        }
    };
    pb::Subject { kind: Some(kind), ..Default::default() }
}
