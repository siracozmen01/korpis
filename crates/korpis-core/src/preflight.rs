// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a preflight reports (§2 of docs/18-operations.md), the node's and the control plane's
//! alike: each requirement, what was found of it, and whether it is met, not needed, lacking
//! with the command that fixes it, or unchecked with what would let it be checked. A report is
//! a failure only when something essential lacks: a machine that lacks the rest offers less.

use std::fmt::Write as _;

/// What a check found of what it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Present as needed.
    Met,
    /// Not needed for what this node is configured to offer, and why.
    NotNeeded(String),
    /// Missing: what the node offers less of without it, and the command that fixes it.
    Lacking {
        /// What is not offered, or not enforced, without it.
        costs: String,
        /// What to run, or do, to have it.
        fix: String,
    },
    /// Not known: why it could not be checked, and what would let it be.
    Unchecked(String),
}

/// One requirement, as found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// What it is.
    pub name: &'static str,
    /// What needs it.
    pub needed_for: &'static str,
    /// What was found, in words.
    pub found: String,
    /// Whether it is there.
    pub outcome: Outcome,
    /// Whether the node can offer anything without it.
    pub essential: bool,
}

/// What preflight found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report(pub Vec<Check>);

impl Report {
    /// Whether the node has anything to offer: nothing essential is lacking.
    #[must_use]
    pub fn offers_anything(&self) -> bool {
        !self.0.iter().any(|c| c.essential && matches!(c.outcome, Outcome::Lacking { .. }))
    }

    /// The report for a person about `subject` ("This node"): each check, what was found, and
    /// what to run where it lacks.
    #[must_use]
    pub fn text(&self, subject: &str) -> String {
        let mut out = String::new();
        for c in &self.0 {
            let mark = match &c.outcome {
                Outcome::Met => "ok",
                Outcome::NotNeeded(_) => "--",
                Outcome::Lacking { .. } if c.essential => "NO",
                Outcome::Lacking { .. } => "no",
                Outcome::Unchecked(_) => "??",
            };
            let _ = writeln!(out, "[{mark}] {}: {} (for {})", c.name, c.found, c.needed_for);
            match &c.outcome {
                Outcome::Met => {}
                Outcome::NotNeeded(why) | Outcome::Unchecked(why) => {
                    let _ = writeln!(out, "     {why}");
                }
                Outcome::Lacking { costs, fix } => {
                    let _ = writeln!(out, "     without it: {costs}\n     to fix: {fix}");
                }
            }
        }
        let verdict = if self.offers_anything() {
            format!("{subject} can run; it offers what is not marked no.")
        } else {
            format!("{subject} cannot run until what is marked NO is fixed.")
        };
        let _ = writeln!(out, "\n{verdict} Nothing on this machine was changed.");
        out
    }

    /// The report for a program.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        let checks: Vec<_> = self.0.iter().map(check_json).collect();
        serde_json::json!({ "offers_anything": self.offers_anything(), "checks": checks })
    }
}

fn check_json(c: &Check) -> serde_json::Value {
    let (status, detail) = match &c.outcome {
        Outcome::Met => ("met", serde_json::json!({})),
        Outcome::NotNeeded(why) => ("not_needed", serde_json::json!({ "why": why })),
        Outcome::Lacking { costs, fix } => {
            ("lacking", serde_json::json!({ "costs": costs, "fix": fix }))
        }
        Outcome::Unchecked(why) => ("unchecked", serde_json::json!({ "why": why })),
    };
    serde_json::json!({
        "name": c.name,
        "needed_for": c.needed_for,
        "found": c.found,
        "essential": c.essential,
        "status": status,
        "detail": detail,
    })
}
