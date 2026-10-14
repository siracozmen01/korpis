// SPDX-License-Identifier: AGPL-3.0-or-later

//! The commands a person types, each a few calls of the API (§2 of docs/13-surface-cli.md).
//! Each answers with the API's own objects, for `--json`, and with a human form; the binary
//! prints whichever was asked for.

pub mod act;
pub mod backup;
pub mod classify;
pub mod clone;
pub mod console;
pub mod event;
pub mod extension;
pub mod get;
pub mod grant;
pub mod logs;
pub mod logs_offloaded;
pub mod provider;
pub mod repository;
pub mod session;
pub mod sets;
pub mod stream;
pub mod upgrade;
pub mod volume;

use crate::complete;
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
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
        #[arg(add = ArgValueCompleter::new(complete::organizations))]
        #[arg(long)]
        organization: Option<String>,
    },
    /// The workloads of a project.
    Workloads {
        #[arg(add = ArgValueCompleter::new(complete::projects))]
        /// The project, by name or identifier.
        project: String,
    },
    /// The volumes of a project, including those whose workload is gone.
    Volumes {
        #[arg(add = ArgValueCompleter::new(complete::projects))]
        /// The project, by name or identifier.
        project: String,
        /// Only those created for or attached to this workload.
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        #[arg(long)]
        workload: Option<String>,
        /// Deleted ones too.
        #[arg(long)]
        deleted: bool,
    },
    /// The nodes of an organization.
    Nodes {
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(add = ArgValueCompleter::new(complete::organizations))]
        #[arg(long)]
        organization: Option<String>,
    },
    /// The plans of a workload.
    Plans {
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        /// The workload: an identifier, `project/name`, or a name.
        workload: String,
    },
    /// The effect log, filtered.
    Effects {
        /// Only this workload's.
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        #[arg(long)]
        workload: Option<String>,
        /// Only this plan's.
        #[arg(add = ArgValueCompleter::new(complete::plans))]
        #[arg(long)]
        plan: Option<String>,
        /// Only this node's.
        #[arg(add = ArgValueCompleter::new(complete::nodes))]
        #[arg(long)]
        node: Option<String>,
    },
}

/// `korpis plan approve|reject|cancel`.
#[derive(Debug, Clone, Subcommand)]
pub enum PlanAction {
    /// Approves, or confirms, a plan.
    Approve {
        #[arg(add = ArgValueCompleter::new(complete::plans))]
        /// The plan.
        plan: String,
    },
    /// Rejects a plan.
    Reject {
        #[arg(add = ArgValueCompleter::new(complete::plans))]
        /// The plan.
        plan: String,
        /// Why, for the record.
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Cancels an approved plan before it starts.
    Cancel {
        #[arg(add = ArgValueCompleter::new(complete::plans))]
        /// The plan.
        plan: String,
    },
}

/// `korpis workload …`.
#[derive(Debug, Clone, Subcommand)]
pub enum WorkloadAction {
    /// Declares it running.
    Start {
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        /// The workload: an identifier, `project/name`, or a name.
        workload: String,
    },
    /// Declares it stopped.
    Stop {
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        /// The workload.
        workload: String,
    },
    /// Declares a restart.
    Restart {
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        /// The workload.
        workload: String,
    },
    /// Places it by hand on a node you choose, overriding the node's cordon and taints and
    /// nothing else (§2.3 of docs/05-scheduling.md).
    Place {
        /// The workload.
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        workload: String,
        /// The node, by name or identifier.
        #[arg(long, add = ArgValueCompleter::new(complete::nodes))]
        node: String,
        /// Why, for the record.
        #[arg(long)]
        reason: String,
    },
    /// Sets how its plan steps are classified: the operator's layer, over the driver's and the
    /// recipe's and never below the vocabulary's floor (§3.3 of docs/01-model.md). The whole
    /// set: none clears it.
    Classify {
        /// The workload.
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        workload: String,
        /// A step action and its class, e.g. `stop=routine`; repeatable.
        #[arg(value_name = "ACTION=disruptive|routine")]
        overrides: Vec<String>,
        /// Why, for the record.
        #[arg(long)]
        reason: String,
    },
    /// Proposes its retirement; its volumes are kept unless named.
    Delete {
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        /// The workload.
        workload: String,
        /// Delete these of its volumes too, by name (`stream_console` for its console stream);
        /// comma-separated. Irreversible: each is its own step, and the plan waits for a person.
        #[arg(long = "delete-volumes", value_delimiter = ',')]
        delete_volumes: Vec<String>,
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
        #[arg(add = ArgValueCompleter::new(complete::organizations))]
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
        #[arg(add = ArgValueCompleter::new(complete::nodes))]
        /// The node, by name or identifier.
        node: String,
    },
    /// Takes work again.
    Uncordon {
        #[arg(add = ArgValueCompleter::new(complete::nodes))]
        /// The node, by name or identifier.
        node: String,
    },
}

/// A set's drift mode, as typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// The out-of-band change stands, and an apply that meets it fails.
    Advisory,
    /// An apply declares over it, on the record.
    Strict,
}

/// `korpis set …` (§5 and §6 of docs/13-surface-cli.md).
#[derive(Debug, Clone, Subcommand)]
pub enum SetAction {
    /// The declared sets of a project.
    List {
        /// The project, by name or identifier.
        #[arg(add = ArgValueCompleter::new(complete::projects))]
        project: String,
    },
    /// Changes what a set does with drift.
    Mode {
        /// The set, as `project/name`.
        set: String,
        /// The mode.
        mode: Mode,
    },
}

/// `korpis grant …` (§7 of docs/13-surface-cli.md).
#[derive(Debug, Clone, Subcommand)]
pub enum GrantAction {
    /// Attenuates a grant you hold into a new one: to a new token by default, whose secret is
    /// printed once, or to `--to`.
    Issue(Box<Issue>),
    /// Revokes a grant and everything attenuated from it.
    Revoke {
        #[arg(add = ArgValueCompleter::new(complete::grants))]
        /// The grant.
        grant: String,
    },
    /// Lists the grants you hold, or those on a scope or held by a subject.
    List {
        /// Only those on this scope, as for `issue`.
        #[arg(add = ArgValueCompleter::new(complete::scopes))]
        #[arg(long, conflicts_with = "to")]
        scope: Option<String>,
        /// Only those held by this subject, as for `issue`.
        #[arg(long)]
        to: Option<String>,
    },
    /// A grant and the chain it was attenuated through.
    Show {
        #[arg(add = ArgValueCompleter::new(complete::grants))]
        /// The grant.
        grant: String,
    },
}

/// `korpis grant issue`.
#[derive(Debug, Clone, clap::Args)]
pub struct Issue {
    /// What it permits, comma-separated, e.g. `workload.read,workload.write`.
    #[arg(long, value_delimiter = ',', required = true)]
    pub actions: Vec<String>,
    /// Where: `org:`, `project:`, `workload:`, or `node:` followed by a name or identifier, or
    /// `volume:`, `endpoint:`, or `stream:` followed by an identifier.
    #[arg(add = ArgValueCompleter::new(complete::scopes))]
    #[arg(long)]
    pub scope: String,
    /// Narrows the scope to what carries this label, `key=value`; repeatable (§3.4 of
    /// docs/08-identity.md).
    #[arg(long = "select")]
    pub select: Vec<String>,
    /// How long it lasts, as `90s`, `30m`, `12h`, or `90d`; as long as its parent otherwise.
    #[arg(long)]
    pub expires: Option<String>,
    /// What it is for, shown wherever it is listed; required for a new token.
    #[arg(long, default_value = "")]
    pub label: String,
    /// Who holds it: `usr_…`, `tok_…`, `wl_…`, `ext_…` (an installed extension),
    /// `ext:PROVIDER:user:ID`, or `ext:PROVIDER:set:REALM:MEMBER`. A new token when absent.
    #[arg(long)]
    pub to: Option<String>,
    /// The grant to attenuate; the one you hold that it is valid from when absent.
    #[arg(add = ArgValueCompleter::new(complete::grants))]
    #[arg(long)]
    pub parent: Option<String>,
    /// Lets the holder pass these of its actions on, comma-separated.
    #[arg(long, value_delimiter = ',')]
    pub delegate: Vec<String>,
    /// How many further levels may exist beneath it when it delegates.
    #[arg(long, default_value_t = 1)]
    pub depth: u32,
    /// A source network it may be used from, as a CIDR; repeatable.
    #[arg(long = "source-cidr")]
    pub source_cidr: Vec<String>,
    /// It may propose, not apply: its plans wait for someone holding `plan.approve` on this
    /// scope, written as for `--scope`.
    #[arg(add = ArgValueCompleter::new(complete::scopes))]
    #[arg(long)]
    pub requires_approval_by: Option<String>,
}

/// `text` as seconds: a whole number followed by `s`, `m`, `h`, or `d`.
pub fn seconds(text: &str) -> Result<i64, crate::error::CliError> {
    let bad = || crate::error::CliError::Local(format!("{text:?}: a number then s, m, h, or d"));
    let unit = text.chars().last().ok_or_else(bad)?;
    let scale = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return Err(bad()),
    };
    let n: i64 = text.trim_end_matches(unit).parse().map_err(|_| bad())?;
    n.checked_mul(scale).filter(|s| *s > 0).ok_or_else(bad)
}

/// `key=value` pairs as a map; `flag` names them in a refusal.
pub fn pairs(
    flag: &str,
    given: &[String],
) -> Result<std::collections::BTreeMap<String, String>, crate::error::CliError> {
    given
        .iter()
        .map(|l| {
            l.split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .ok_or_else(|| crate::error::CliError::Local(format!("{flag} {l:?}: key=value")))
        })
        .collect()
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
