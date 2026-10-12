// SPDX-License-Identifier: AGPL-3.0-or-later

//! Scopes, conditions and delegation policies, as stored in the `grants` table.

use super::{from_time, to_time};
use chrono::{DateTime, Utc};
use korpis_app::ports::StoreError;
use korpis_core::authz::scope::LabelExpression;
use korpis_core::authz::{Action, Conditions, Delegation, Kind, LabelSelector, ObjectRef, Scope};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn corrupt(what: &str, detail: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(format!("{what}: {detail}"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ObjectDoc {
    kind: String,
    id: Uuid,
}

impl ObjectDoc {
    fn from_domain(o: ObjectRef) -> Self {
        Self { kind: o.kind.as_str().to_owned(), id: o.id }
    }

    fn into_domain(self) -> Result<ObjectRef, StoreError> {
        let kind = Kind::parse(&self.kind).ok_or_else(|| corrupt("scope kind", &self.kind))?;
        Ok(ObjectRef::new(kind, self.id))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum ExpressionDoc {
    In { key: String, values: Vec<String> },
    NotIn { key: String, values: Vec<String> },
    Exists { key: String },
    DoesNotExist { key: String },
}

/// A scope: one object, or the objects below one that match a selector.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ScopeDoc {
    Object {
        object: ObjectDoc,
    },
    Selector {
        within: ObjectDoc,
        match_labels: BTreeMap<String, String>,
        expressions: Vec<ExpressionDoc>,
    },
}

impl ScopeDoc {
    pub(crate) fn from_domain(scope: &Scope) -> Self {
        match scope {
            Scope::Object(o) => Self::Object { object: ObjectDoc::from_domain(*o) },
            Scope::Selector { within, selector } => Self::Selector {
                within: ObjectDoc::from_domain(*within),
                match_labels: selector.match_labels.clone(),
                expressions: selector.expressions.iter().map(expression_doc).collect(),
            },
        }
    }

    pub(crate) fn into_domain(self) -> Result<Scope, StoreError> {
        Ok(match self {
            Self::Object { object } => Scope::Object(object.into_domain()?),
            Self::Selector { within, match_labels, expressions } => Scope::Selector {
                within: within.into_domain()?,
                selector: LabelSelector {
                    match_labels,
                    expressions: expressions.into_iter().map(expression).collect(),
                },
            },
        })
    }
}

fn expression_doc(e: &LabelExpression) -> ExpressionDoc {
    match e.clone() {
        LabelExpression::In(key, values) => ExpressionDoc::In { key, values },
        LabelExpression::NotIn(key, values) => ExpressionDoc::NotIn { key, values },
        LabelExpression::Exists(key) => ExpressionDoc::Exists { key },
        LabelExpression::DoesNotExist(key) => ExpressionDoc::DoesNotExist { key },
    }
}

fn expression(e: ExpressionDoc) -> LabelExpression {
    match e {
        ExpressionDoc::In { key, values } => LabelExpression::In(key, values),
        ExpressionDoc::NotIn { key, values } => LabelExpression::NotIn(key, values),
        ExpressionDoc::Exists { key } => LabelExpression::Exists(key),
        ExpressionDoc::DoesNotExist { key } => LabelExpression::DoesNotExist(key),
    }
}

/// A grant's conditions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ConditionsDoc {
    expires_at: Option<DateTime<Utc>>,
    not_before: Option<DateTime<Utc>>,
    source_cidr: Vec<String>,
    requires_mfa: bool,
    requires_approval_by: Option<ScopeDoc>,
    max_uses: Option<u64>,
}

impl ConditionsDoc {
    pub(crate) fn from_domain(c: &Conditions) -> Self {
        Self {
            expires_at: c.expires_at.map(from_time),
            not_before: c.not_before.map(from_time),
            source_cidr: c.source_cidr.iter().map(ToString::to_string).collect(),
            requires_mfa: c.requires_mfa,
            requires_approval_by: c.requires_approval_by.as_ref().map(ScopeDoc::from_domain),
            max_uses: c.max_uses,
        }
    }

    pub(crate) fn into_domain(self) -> Result<Conditions, StoreError> {
        let source_cidr = self
            .source_cidr
            .iter()
            .map(|n| n.parse().map_err(|_| corrupt("source_cidr", n)))
            .collect::<Result<_, _>>()?;
        Ok(Conditions {
            expires_at: self.expires_at.map(to_time),
            not_before: self.not_before.map(to_time),
            source_cidr,
            requires_mfa: self.requires_mfa,
            requires_approval_by: self
                .requires_approval_by
                .map(ScopeDoc::into_domain)
                .transpose()?,
            max_uses: self.max_uses,
        })
    }
}

/// A grant's delegation policy. `allowed` is read by a CHECK constraint of the schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DelegationDoc {
    allowed: bool,
    max_depth: Option<u32>,
    actions: Vec<String>,
}

impl DelegationDoc {
    pub(crate) fn from_domain(d: &Delegation) -> Self {
        Self {
            allowed: d.allowed,
            max_depth: d.max_depth,
            actions: d.actions.iter().map(|a| a.as_str().to_owned()).collect(),
        }
    }

    pub(crate) fn into_domain(self) -> Result<Delegation, StoreError> {
        Ok(Delegation {
            allowed: self.allowed,
            max_depth: self.max_depth,
            actions: actions(&self.actions)?,
        })
    }
}

/// Stored action names.
pub(crate) fn actions(names: &[String]) -> Result<BTreeSet<Action>, StoreError> {
    names.iter().map(|n| Action::parse(n).map_err(|e| corrupt("action", e))).collect()
}
