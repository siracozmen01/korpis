// SPDX-License-Identifier: AGPL-3.0-or-later

//! Explanations and placement policies, as stored in the `placement` and `placement_failure`
//! tables: complete, whoever later reads them (§8 of docs/08-identity.md).

use super::SelectorDoc;
use korpis_app::ports::StoreError;
use korpis_core::schedule::{
    Check, Explanation, PlacementPolicy, Rejection, Score, Signal, SpreadConstraint, SpreadScope,
    Strategy,
};
use korpis_core::workload::spec::placement::AffinityRule;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

fn corrupt(what: &str, s: &str) -> StoreError {
    StoreError::Corrupt(format!("explanation {what} {s:?}"))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StrategyDoc {
    Packed,
    Distributed,
    Balanced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SpreadScopeDoc {
    Project,
    Organization,
    Selector { selector: SelectorDoc },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpreadDoc {
    topology_key: String,
    max_skew: u32,
    scope: SpreadScopeDoc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AffinityDoc {
    anti: bool,
    required: bool,
    workloads: SelectorDoc,
    topology_key: String,
}

impl AffinityDoc {
    pub(crate) fn from_domain(a: &AffinityRule) -> Self {
        Self {
            anti: a.anti,
            required: a.required,
            workloads: SelectorDoc::from_domain(&a.workloads),
            topology_key: a.topology_key.clone(),
        }
    }

    pub(crate) fn into_domain(self) -> AffinityRule {
        AffinityRule {
            anti: self.anti,
            required: self.required,
            workloads: self.workloads.into_domain(),
            topology_key: self.topology_key,
        }
    }
}

/// A placement policy as stored: in an explanation, and as an organization's own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PolicyDoc {
    strategy: StrategyDoc,
    spread: Vec<SpreadDoc>,
    affinity: Vec<AffinityDoc>,
    weights: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RejectionDoc {
    node: Uuid,
    check: String,
    detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScoreDoc {
    node: Uuid,
    signal: String,
    value: f64,
    weight: f64,
}

/// An explanation as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExplanationDoc {
    candidates: u32,
    filtered: Vec<RejectionDoc>,
    scored: Vec<ScoreDoc>,
    chosen: Option<Uuid>,
    runner_up: Option<Uuid>,
    policy: PolicyDoc,
    /// Absent from an explanation stored before organizations set policies, when every node
    /// was scored by the one policy.
    #[serde(default)]
    owners: BTreeMap<Uuid, PolicyDoc>,
}

impl PolicyDoc {
    pub(crate) fn from_domain(p: &PlacementPolicy) -> Self {
        Self {
            strategy: match p.strategy {
                Strategy::Packed => StrategyDoc::Packed,
                Strategy::Distributed => StrategyDoc::Distributed,
                Strategy::Balanced => StrategyDoc::Balanced,
            },
            spread: p
                .spread
                .iter()
                .map(|s| SpreadDoc {
                    topology_key: s.topology_key.clone(),
                    max_skew: s.max_skew,
                    scope: match &s.scope {
                        SpreadScope::Project => SpreadScopeDoc::Project,
                        SpreadScope::Organization => SpreadScopeDoc::Organization,
                        SpreadScope::Selector(sel) => {
                            SpreadScopeDoc::Selector { selector: SelectorDoc::from_domain(sel) }
                        }
                    },
                })
                .collect(),
            affinity: p.affinity.iter().map(AffinityDoc::from_domain).collect(),
            weights: p.weights.iter().map(|(s, w)| (s.as_str().to_owned(), *w)).collect(),
        }
    }

    pub(crate) fn into_domain(self) -> Result<PlacementPolicy, StoreError> {
        Ok(PlacementPolicy {
            strategy: match self.strategy {
                StrategyDoc::Packed => Strategy::Packed,
                StrategyDoc::Distributed => Strategy::Distributed,
                StrategyDoc::Balanced => Strategy::Balanced,
            },
            spread: self
                .spread
                .into_iter()
                .map(|s| SpreadConstraint {
                    topology_key: s.topology_key,
                    max_skew: s.max_skew,
                    scope: match s.scope {
                        SpreadScopeDoc::Project => SpreadScope::Project,
                        SpreadScopeDoc::Organization => SpreadScope::Organization,
                        SpreadScopeDoc::Selector { selector } => {
                            SpreadScope::Selector(selector.into_domain())
                        }
                    },
                })
                .collect(),
            affinity: self.affinity.into_iter().map(AffinityDoc::into_domain).collect(),
            weights: self
                .weights
                .into_iter()
                .map(|(k, w)| Ok((Signal::parse(&k).ok_or_else(|| corrupt("signal", &k))?, w)))
                .collect::<Result<_, StoreError>>()?,
        })
    }
}

impl ExplanationDoc {
    pub(crate) fn from_domain(e: &Explanation) -> Self {
        Self {
            candidates: e.candidates,
            filtered: e
                .filtered
                .iter()
                .map(|r| RejectionDoc {
                    node: r.node,
                    check: r.check.as_str().to_owned(),
                    detail: r.detail.clone(),
                })
                .collect(),
            scored: e
                .scored
                .iter()
                .map(|s| ScoreDoc {
                    node: s.node,
                    signal: s.signal.as_str().to_owned(),
                    value: s.value,
                    weight: s.weight,
                })
                .collect(),
            chosen: e.chosen,
            runner_up: e.runner_up,
            policy: PolicyDoc::from_domain(&e.policy),
            owners: e.owners.iter().map(|(o, p)| (*o, PolicyDoc::from_domain(p))).collect(),
        }
    }

    pub(crate) fn into_domain(self) -> Result<Explanation, StoreError> {
        let filtered = self
            .filtered
            .into_iter()
            .map(|r| {
                let check = Check::parse(&r.check).ok_or_else(|| corrupt("check", &r.check))?;
                Ok(Rejection { node: r.node, check, detail: r.detail })
            })
            .collect::<Result<_, StoreError>>()?;
        let scored = self
            .scored
            .into_iter()
            .map(|s| {
                let signal =
                    Signal::parse(&s.signal).ok_or_else(|| corrupt("signal", &s.signal))?;
                Ok(Score { node: s.node, signal, value: s.value, weight: s.weight })
            })
            .collect::<Result<_, StoreError>>()?;
        Ok(Explanation {
            candidates: self.candidates,
            filtered,
            scored,
            chosen: self.chosen,
            runner_up: self.runner_up,
            policy: self.policy.into_domain()?,
            owners: self
                .owners
                .into_iter()
                .map(|(o, p)| Ok((o, p.into_domain()?)))
                .collect::<Result<_, StoreError>>()?,
        })
    }
}
