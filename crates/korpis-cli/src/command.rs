// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `korpis` command line's commands (§2 of docs/13-surface-cli.md): what each is called, the
//! help it prints, and the arguments it takes. The binary parses them and runs each.

use crate::cmd::backup::BackupAction;
use crate::cmd::certificate::CertificateAction;
use crate::cmd::domain::DomainAction;
use crate::cmd::event::EventAction;
use crate::cmd::extension::ExtensionAction;
use crate::cmd::identity::IdentityAction;
use crate::cmd::portal::PortalAction;
use crate::cmd::quota::QuotaAction;
use crate::cmd::repository::RepositoryAction;
use crate::cmd::run::RunAction;
use crate::cmd::template::TemplateAction;
use crate::cmd::tenancy::{OrganizationAction, ProjectAction};
use crate::cmd::volume::VolumeAction;
use crate::cmd::{Get, GrantAction, NodeAction, PlanAction, SetAction, WorkloadAction};
use crate::cmd::{console, layout, logs, stream_share, theme};
use crate::complete::{self, Shell};
use crate::sets::SetArgs;
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use std::path::PathBuf;

/// What `korpis` is asked to do: one variant per command, each the help text clap prints.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Declares the files and shows the real plan, persisted and waiting to be applied; or
    /// approves, rejects, or cancels a plan.
    #[command(args_conflicts_with_subcommands = true)]
    Plan {
        /// A declared file, or a directory of them; repeatable.
        #[arg(short = 'f', long = "file")]
        files: Vec<PathBuf>,
        /// The set the files declare, if they declare one.
        #[command(flatten)]
        set: SetArgs,
        /// What to do with a plan already shown, in place of declaring files.
        #[command(subcommand)]
        action: Option<PlanAction>,
    },
    /// Declares the files and applies what default policy approves; with `--plan`, applies
    /// exactly the plans named, if the files still declare what they planned.
    Apply {
        /// A declared file, or a directory of them; repeatable.
        #[arg(short = 'f', long = "file", required = true)]
        files: Vec<PathBuf>,
        /// A plan `korpis plan` showed; repeatable, one per workload.
        #[arg(long = "plan")]
        plans: Vec<String>,
        /// The set the files declare, if they declare one.
        #[command(flatten)]
        set: SetArgs,
    },
    /// Makes and shows organizations; `get organizations` lists them.
    #[command(subcommand)]
    Organization(OrganizationAction),
    /// Makes projects; `get projects` lists them.
    #[command(subcommand)]
    Project(ProjectAction),
    /// Shows and sets quota sets, from above.
    #[command(subcommand)]
    Quota(QuotaAction),
    /// Grant templates: the roles of the interface, applied with `grant issue --template`.
    #[command(subcommand)]
    Template(TemplateAction),
    /// Identity bindings: the people and sets at an identity provider that grants can name.
    #[command(subcommand)]
    Identity(IdentityAction),
    /// Lists what you can read.
    #[command(subcommand)]
    Get(Get),
    /// A workload in detail: where it runs, what it is doing, and what it waits for.
    Status {
        /// The workload: an identifier, `project/name`, or a name.
        #[arg(add = ArgValueCompleter::new(complete::workloads))]
        workload: String,
    },
    /// Starts, stops, restarts, or deletes a workload.
    #[command(subcommand)]
    Workload(WorkloadAction),
    /// Enrolls, cordons, or uncordons a node.
    #[command(subcommand)]
    Node(NodeAction),
    /// Deletes a retained volume; takes, lists, rolls back to, and deletes snapshots.
    #[command(subcommand)]
    Volume(VolumeAction),
    /// Makes, lists, and shows backup repositories.
    #[command(subcommand)]
    Repository(RepositoryAction),
    /// Uploads, replaces, lists, shows, and deletes the certificates an organization's endpoints
    /// serve with.
    #[command(subcommand)]
    Certificate(CertificateAction),
    /// Asks for, verifies, lists, shows, and deletes the domains an organization holds.
    #[command(subcommand)]
    Domain(DomainAction),
    /// Makes, changes, lists, shows, and deletes the names the control plane answers under for
    /// an organization, with its theme and sign-in.
    #[command(subcommand)]
    Portal(PortalAction),
    /// Shows, sets, and clears an organization's theme in the panel.
    #[command(subcommand)]
    Theme(theme::ThemeAction),
    /// Shows, sets, and clears the landing screen an organization composes in the panel.
    #[command(subcommand)]
    Layout(layout::LayoutAction),
    /// Takes, lists, and shows backups.
    #[command(subcommand)]
    Backup(BackupAction),
    /// Lists and shows a task or scheduled workload's runs, and cancels one.
    #[command(subcommand)]
    Run(RunAction),
    /// Issues, revokes, lists, and shows grants.
    #[command(subcommand)]
    Grant(GrantAction),
    /// Lists a project's declared sets, or changes what one does with drift.
    #[command(subcommand)]
    Set(SetAction),
    /// Installs, lists, shows, and uninstalls extensions.
    #[command(subcommand)]
    Extension(ExtensionAction),
    /// Subscribes to the event log, and follows a subscription.
    #[command(subcommand)]
    Event(EventAction),
    /// A workload's console, read without attaching: from the oldest record its node holds,
    /// an offset, or a time ago; searched; followed as it is written.
    Logs(logs::Logs),
    /// A workload's console stream as the control plane indexes it: how long it is kept, on the
    /// node and offloaded, the operator's floor, what is held only on the node, what is lost;
    /// `share` makes a link to read a range of it.
    Stream(stream_share::StreamArgs),
    /// A workload's files through a relay: ls, get, put, sync, mkdir, mv, and rm.
    #[command(subcommand)]
    Files(crate::cmd::files::FilesAction),
    /// Attaches to a workload's console: its scrollback, then what it writes, and, where you may
    /// type, your keys; `Ctrl-]` detaches.
    Console(console::Console),
    /// Prints the script that registers completion with a shell, e.g.
    /// `source <(korpis completion bash)`; completion then asks this binary on every key, so it
    /// offers only what the token in the environment can read.
    Completion {
        /// The shell.
        shell: Shell,
    },
    /// The client's version and source revision, and the server's when one is named.
    Version,
    /// Signs in by approving a code in the panel, and keeps the session in the system keyring,
    /// or where there is none in a file only you can read.
    Login {
        /// Keep nothing: sign in, run the command after `--` with the session in memory, and
        /// sign out.
        #[arg(long, requires = "command")]
        no_store: bool,
        /// With `--no-store`, the command to run, e.g. `-- get workloads`.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Ends the session `korpis login` began, and forgets it.
    Logout,
    /// Calls any method of the API by name, e.g. `WorkloadService/GetWorkload`.
    Call {
        /// The method, as `Service/Method` or `korpis.v1.Service/Method`.
        method: Option<String>,
        /// The request as JSON, `@FILE` to read it from a file, or `-` for standard input.
        #[arg(long, short, default_value = "{}")]
        data: String,
        /// List every method instead.
        #[arg(long)]
        list: bool,
    },
}
