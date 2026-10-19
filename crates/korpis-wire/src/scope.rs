// SPDX-License-Identifier: AGPL-3.0-or-later

//! Scopes and label selectors.

use crate::invalid;
use crate::{id, id_kind, wire_id};

use korpis_core::authz::scope::LabelExpression;
use korpis_core::authz::{Kind, LabelSelector, ObjectRef, Scope};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::scope::Kind as K;

/// A scope from the request field `field`.
pub fn from_pb(field: &str, scope: &pb::Scope) -> Result<Scope, crate::WireError> {
    match scope.kind.as_ref().ok_or_else(|| invalid(field, "required"))? {
        K::Selector(sel) => {
            let within_field = format!("{field}.selector.within");
            let within =
                sel.within.as_option().ok_or_else(|| invalid(&within_field, "required"))?;
            Ok(Scope::Selector {
                within: object_from_pb(&within_field, within)?,
                selector: selector_from_pb(
                    &format!("{field}.selector.selector"),
                    sel.selector.as_option(),
                )?,
            })
        }
        other => Ok(Scope::Object(object_from_kind(field, other)?)),
    }
}

/// An object scope from the request field `field`; selectors are refused.
pub fn object_from_pb(field: &str, scope: &pb::Scope) -> Result<ObjectRef, crate::WireError> {
    object_from_kind(field, scope.kind.as_ref().ok_or_else(|| invalid(field, "required"))?)
}

fn object_from_kind(field: &str, kind: &K) -> Result<ObjectRef, crate::WireError> {
    let (k, value) = match kind {
        K::Organization(v) => (Kind::Organization, v),
        K::Project(v) => (Kind::Project, v),
        K::Workload(v) => (Kind::Workload, v),
        K::Volume(v) => (Kind::Volume, v),
        K::Endpoint(v) => (Kind::Endpoint, v),
        K::Stream(v) => (Kind::Stream, v),
        K::Node(v) => (Kind::Node, v),
        K::Repository(v) => (Kind::Repository, v),
        K::Domain(v) => (Kind::Domain, v),
        K::Certificate(v) => (Kind::Certificate, v),
        K::Selector(_) => return Err(invalid(field, "expected an object, not a selector")),
    };
    Ok(ObjectRef::new(k, id(id_kind(k), field, value)?))
}

/// The wire form of an object scope.
#[must_use]
pub fn object_to_pb(o: ObjectRef) -> pb::Scope {
    let id = wire_id(id_kind(o.kind), o.id);
    let kind = match o.kind {
        Kind::Organization => K::Organization(id),
        Kind::Project => K::Project(id),
        Kind::Workload => K::Workload(id),
        Kind::Volume => K::Volume(id),
        Kind::Endpoint => K::Endpoint(id),
        Kind::Stream => K::Stream(id),
        Kind::Node => K::Node(id),
        Kind::Repository => K::Repository(id),
        Kind::Domain => K::Domain(id),
        Kind::Certificate => K::Certificate(id),
    };
    pb::Scope { kind: Some(kind), ..Default::default() }
}

/// The wire form of a scope.
#[must_use]
pub fn to_pb(scope: &Scope) -> pb::Scope {
    match scope {
        Scope::Object(o) => object_to_pb(*o),
        Scope::Selector { within, selector } => pb::Scope {
            kind: Some(K::Selector(Box::new(pb::SelectorScope {
                within: object_to_pb(*within).into(),
                selector: selector_to_pb(selector).into(),
                ..Default::default()
            }))),
            ..Default::default()
        },
    }
}

/// A label selector from the field `field`; absent selects everything.
pub fn selector_from_pb(
    field: &str,
    sel: Option<&pb::LabelSelector>,
) -> Result<LabelSelector, crate::WireError> {
    let Some(sel) = sel else { return Ok(LabelSelector::default()) };
    let expressions = sel
        .match_expressions
        .iter()
        .map(|e| {
            let (key, values) = (e.key.clone(), e.values.clone());
            Ok(match e.operator.as_known() {
                Some(pb::LabelOperator::LABEL_OPERATOR_IN) => LabelExpression::In(key, values),
                Some(pb::LabelOperator::LABEL_OPERATOR_NOT_IN) => {
                    LabelExpression::NotIn(key, values)
                }
                Some(pb::LabelOperator::LABEL_OPERATOR_EXISTS) => LabelExpression::Exists(key),
                Some(pb::LabelOperator::LABEL_OPERATOR_DOES_NOT_EXIST) => {
                    LabelExpression::DoesNotExist(key)
                }
                _ => {
                    return Err(invalid(&format!("{field}.match_expressions"), "unknown operator"));
                }
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(LabelSelector {
        match_labels: sel.match_labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        expressions,
    })
}

fn expression_to_pb(e: &LabelExpression) -> pb::LabelExpression {
    use pb::LabelOperator as Op;
    let (operator, key, values) = match e {
        LabelExpression::In(k, v) => (Op::LABEL_OPERATOR_IN, k, v.clone()),
        LabelExpression::NotIn(k, v) => (Op::LABEL_OPERATOR_NOT_IN, k, v.clone()),
        LabelExpression::Exists(k) => (Op::LABEL_OPERATOR_EXISTS, k, Vec::new()),
        LabelExpression::DoesNotExist(k) => (Op::LABEL_OPERATOR_DOES_NOT_EXIST, k, Vec::new()),
    };
    pb::LabelExpression {
        key: key.clone(),
        operator: operator.into(),
        values,
        ..Default::default()
    }
}

/// The wire form of a label selector.
#[must_use]
pub fn selector_to_pb(sel: &LabelSelector) -> pb::LabelSelector {
    pb::LabelSelector {
        match_labels: sel.match_labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        match_expressions: sel.expressions.iter().map(expression_to_pb).collect(),
        ..Default::default()
    }
}

/// The wire form of an object kind.
#[must_use]
pub fn object_kind_to_pb(k: Kind) -> pb::ObjectKind {
    match k {
        Kind::Organization => pb::ObjectKind::OBJECT_KIND_ORGANIZATION,
        Kind::Project => pb::ObjectKind::OBJECT_KIND_PROJECT,
        Kind::Workload => pb::ObjectKind::OBJECT_KIND_WORKLOAD,
        Kind::Volume => pb::ObjectKind::OBJECT_KIND_VOLUME,
        Kind::Endpoint => pb::ObjectKind::OBJECT_KIND_ENDPOINT,
        Kind::Stream => pb::ObjectKind::OBJECT_KIND_STREAM,
        Kind::Node => pb::ObjectKind::OBJECT_KIND_NODE,
        Kind::Repository => pb::ObjectKind::OBJECT_KIND_REPOSITORY,
        Kind::Domain => pb::ObjectKind::OBJECT_KIND_DOMAIN,
        Kind::Certificate => pb::ObjectKind::OBJECT_KIND_CERTIFICATE,
    }
}

/// An object kind from the field `field`.
pub fn object_kind_from_pb(
    field: &str,
    k: buffa::EnumValue<pb::ObjectKind>,
) -> Result<Kind, crate::WireError> {
    use pb::ObjectKind as K;
    Ok(match k.as_known() {
        Some(K::OBJECT_KIND_ORGANIZATION) => Kind::Organization,
        Some(K::OBJECT_KIND_PROJECT) => Kind::Project,
        Some(K::OBJECT_KIND_WORKLOAD) => Kind::Workload,
        Some(K::OBJECT_KIND_VOLUME) => Kind::Volume,
        Some(K::OBJECT_KIND_ENDPOINT) => Kind::Endpoint,
        Some(K::OBJECT_KIND_STREAM) => Kind::Stream,
        Some(K::OBJECT_KIND_NODE) => Kind::Node,
        Some(K::OBJECT_KIND_REPOSITORY) => Kind::Repository,
        Some(K::OBJECT_KIND_DOMAIN) => Kind::Domain,
        Some(K::OBJECT_KIND_CERTIFICATE) => Kind::Certificate,
        _ => return Err(invalid(field, "expected an object kind")),
    })
}

/// An object from the field `field`, its identifier of its kind.
pub fn object_ref_from_pb(field: &str, o: &pb::ObjectRef) -> Result<ObjectRef, crate::WireError> {
    let kind = object_kind_from_pb(&format!("{field}.kind"), o.kind)?;
    Ok(ObjectRef::new(kind, crate::id(crate::id_kind(kind), &format!("{field}.id"), &o.id)?))
}
