// SPDX-License-Identifier: AGPL-3.0-or-later

//! The commands a person types, each a few calls of the API (§2 of docs/13-surface-cli.md).
//! Each answers with the API's own objects, for `--json`, and with a human form; the binary
//! prints whichever was asked for.

pub mod act;
pub mod get;

use clap::Subcommand;
use serde_json::Value;

/// What a command answers.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// The API's own objects.
    pub json: Value,
    /// The same, for a person.
    pub human: String,
}

/// `korpis get`.
#[derive(Debug, Clone, Subcommand)]
pub enum Get {
    /// The organizations you can read.
    Organizations,
    /// The projects of an organization.
    Projects {
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
    },
    /// The workloads of a project.
    Workloads {
        /// The project, by name or identifier.
        project: String,
    },
    /// The nodes of an organization.
    Nodes {
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
    },
    /// The plans of a workload.
    Plans {
        /// The workload: an identifier, `project/name`, or a name.
        workload: String,
    },
    /// The effect log, filtered.
    Effects {
        /// Only this workload's.
        #[arg(long)]
        workload: Option<String>,
        /// Only this plan's.
        #[arg(long)]
        plan: Option<String>,
        /// Only this node's.
        #[arg(long)]
        node: Option<String>,
    },
}

/// `korpis plan approve|reject|cancel`.
#[derive(Debug, Clone, Subcommand)]
pub enum PlanAction {
    /// Approves, or confirms, a plan.
    Approve {
        /// The plan.
        plan: String,
    },
    /// Rejects a plan.
    Reject {
        /// The plan.
        plan: String,
        /// Why, for the record.
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Cancels an approved plan before it starts.
    Cancel {
        /// The plan.
        plan: String,
    },
}

/// `korpis workload …`.
#[derive(Debug, Clone, Subcommand)]
pub enum WorkloadAction {
    /// Declares it running.
    Start {
        /// The workload: an identifier, `project/name`, or a name.
        workload: String,
    },
    /// Declares it stopped.
    Stop {
        /// The workload.
        workload: String,
    },
    /// Declares a restart.
    Restart {
        /// The workload.
        workload: String,
    },
    /// Proposes its retirement; its volumes are kept.
    Delete {
        /// The workload.
        workload: String,
    },
}

/// `korpis node token …`.
#[derive(Debug, Clone, Subcommand)]
pub enum TokenAction {
    /// Creates a one-time enrollment token for a node (§3 of docs/18-operations.md).
    Create {
        /// The node's name, unique in the installation.
        name: String,
        /// The organization it joins; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
        /// A label, `key=value`; repeatable.
        #[arg(long = "label")]
        labels: Vec<String>,
        /// How long the token may be redeemed, as `90s`, `30m`, or `1h`; an hour by default,
        /// and at most a day.
        #[arg(long)]
        expires: Option<String>,
    },
}

/// `korpis node …`.
#[derive(Debug, Clone, Subcommand)]
pub enum NodeAction {
    /// Enrollment tokens.
    #[command(subcommand)]
    Token(TokenAction),
    /// Takes nothing new.
    Cordon {
        /// The node, by name or identifier.
        node: String,
    },
    /// Takes work again.
    Uncordon {
        /// The node, by name or identifier.
        node: String,
    },
}

/// A fresh mutation, with `expected` as its compare-and-swap version.
#[must_use]
pub fn mutation(expected: Option<u64>) -> korpis_proto::korpis::v1::Mutation {
    korpis_proto::korpis::v1::Mutation {
        idempotency_key: uuid::Uuid::now_v7().to_string(),
        expected_version: expected,
        ..Default::default()
    }
}

/// `message` as JSON.
pub fn json<T: serde::Serialize>(message: &T) -> Value {
    serde_json::to_value(message).unwrap_or(Value::Null)
}
