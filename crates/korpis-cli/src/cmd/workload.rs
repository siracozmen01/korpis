// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis workload …`: the imperative commands on one workload (§2 of docs/13-surface-cli.md).

use crate::complete;
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;

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
    /// The recipe it runs: what it is, the configuration it declares, and the words it shows,
    /// in your language where the recipe speaks it (§5.3 of docs/09-recipes.md).
    Recipe {
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        /// The workload.
        workload: String,
        /// A language to read it in, as a BCP 47 tag such as `tr` or `pt-BR`; repeatable, most
        /// preferred first. By default the one `LC_ALL`, `LC_MESSAGES`, or `LANG` names.
        #[arg(long = "locale")]
        locales: Vec<String>,
    },
}
