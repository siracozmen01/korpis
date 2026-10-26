// SPDX-License-Identifier: AGPL-3.0-or-later

//! The commands a person types, each a few calls of the API (§2 of docs/13-surface-cli.md).
//! Each answers with the API's own objects, for `--json`, and with a human form; the binary
//! prints whichever was asked for.

pub mod act;
pub mod address_pool;
pub mod backup;
pub mod ceiling;
pub mod certificate;
pub mod classify;
pub mod clone;
pub mod console;
pub mod domain;
pub mod event;
pub mod extension;
pub mod files;
pub mod get;
pub mod grant;
pub mod identity;
pub mod layout;
pub mod logs;
pub mod logs_offloaded;
pub mod placement;
pub mod portal;
pub mod provider;
pub mod quota;
pub mod recipe;
pub mod repository;
pub mod run;
pub mod session;
pub mod sets;
pub mod sftp;
pub mod share;
pub mod ssh_key;
pub mod stream;
pub mod stream_share;
pub mod template;
pub mod tenancy;
pub mod theme;
pub mod upgrade;
pub mod usage;
pub mod volume;
pub mod workload;

pub use workload::WorkloadAction;

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
    /// The organizations you can read, or those directly beneath one of them.
    Organizations {
        /// Only those directly beneath this organization, by name or identifier.
        #[arg(long, add = ArgValueCompleter::new(complete::organizations))]
        parent: Option<String>,
    },
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
    /// The effect log, as far as the caller's grants reach: an organization's subtree (the
    /// caller's own when nothing is named), or one project, workload, plan, or node.
    Effects(Effects),
    /// What workloads used over intervals in a window, as their nodes measured it, and where
    /// nothing was heard (§2 of docs/15-observability.md).
    Usage(usage::Usage),
    /// The metering days in a window, and whether each has closed.
    Periods(usage::Periods),
}

/// What `korpis get effects` reads.
#[derive(Debug, Clone, clap::Args)]
pub struct Effects {
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
    /// What is about anything in this project.
    #[arg(add = ArgValueCompleter::new(complete::projects))]
    #[arg(long)]
    project: Option<String>,
    /// What is about anything in this organization's subtree.
    #[arg(long = "org", add = ArgValueCompleter::new(complete::organizations))]
    organization: Option<String>,
    /// Only actions beginning with this, such as `grant.` or `workload.restart`.
    #[arg(long)]
    action: Option<String>,
    /// At most this many, the oldest first.
    #[arg(long, default_value_t = 100)]
    limit: usize,
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
    /// Retires a node for good: once cordoned and empty, or at once as compromised.
    Remove {
        #[arg(add = ArgValueCompleter::new(complete::nodes))]
        /// The node, by name or identifier.
        node: String,
        /// It is compromised: remove it whatever it holds, placing its workloads again from
        /// their targets; its volumes are lost with it.
        #[arg(long)]
        compromised: bool,
        /// Removing a node cannot be undone; say so.
        #[arg(long)]
        yes: bool,
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
    /// Makes a share link: a grant whoever holds the link holds, on one object, expiring and
    /// limited in uses, which can never make another (§6.1 of docs/08-identity.md).
    Share(Box<share::Share>),
}

/// `korpis grant issue`.
#[derive(Debug, Clone, clap::Args)]
pub struct Issue {
    /// What it permits, comma-separated, e.g. `workload.read,workload.write`.
    #[arg(long, value_delimiter = ',', required_unless_present = "template")]
    pub actions: Vec<String>,
    /// Issues from a grant template, `tpl_…` or a name offered at `--scope`, which says the
    /// actions and the delegation and is applied at the object `--scope` names (§4 of
    /// docs/08-identity.md); `--expires`, `--source-cidr`, and `--requires-approval-by` then
    /// replace its defaults.
    #[arg(long, conflicts_with_all = ["actions", "delegate", "select"])]
    pub template: Option<String>,
    /// The template's version to issue; the one it has now by default.
    #[arg(long = "template-version", requires = "template")]
    pub template_version: Option<u64>,
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
    /// `ext:org_…/PROVIDER:user:ID`, or `ext:org_…/PROVIDER:set:REALM:MEMBER` (an identity
    /// provider's subject, bound in its organization). A new token when absent.
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
