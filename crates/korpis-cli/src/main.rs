// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis`: the reference client.

use clap::{CommandFactory as _, Parser, Subcommand};
use clap_complete::CompleteEnv;
use clap_complete::engine::ArgValueCompleter;
use korpis_cli::call::{METHODS, call};
use korpis_cli::cmd::backup::{self, BackupAction};
use korpis_cli::cmd::certificate::{self, CertificateAction};
use korpis_cli::cmd::clone;
use korpis_cli::cmd::domain::{self, DomainAction};
use korpis_cli::cmd::event::{self, EventAction};
use korpis_cli::cmd::extension::{self, ExtensionAction};
use korpis_cli::cmd::files::files as remote_files;
use korpis_cli::cmd::repository::{self, RepositoryAction};
use korpis_cli::cmd::volume::{self, VolumeAction};
use korpis_cli::cmd::{
    Answer, Get, GrantAction, NodeAction, PlanAction, SetAction, WorkloadAction, act, get, grant,
    sets,
};
use korpis_cli::cmd::{console, logs, stream_share, theme};
use korpis_cli::complete::{self, COMPLETE_VAR, Shell, registration};
use korpis_cli::connect::{Connection, Endpoint, token_from};
use korpis_cli::error::CliError;
use korpis_cli::files;
use korpis_cli::session::{self, Held};
use korpis_cli::sets::SetArgs;
use korpis_cli::vault::Vault;
use korpis_proto::korpis::v1 as pb;
use korpis_recipe::registry::Registry;
use serde_json::Value;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "korpis", version = korpis_core::build::version_line())]
struct Cli {
    /// The control plane, e.g. `https://korpis.example:7443`.
    #[arg(long, env = "KORPIS_SERVER", global = true)]
    server: Option<String>,
    /// The installation CA in PEM, the one certificate authority trusted for the server.
    #[arg(long, env = "KORPIS_CA", global = true)]
    ca: Option<PathBuf>,
    /// A file holding the capability token; otherwise `KORPIS_TOKEN`.
    #[arg(long, env = "KORPIS_TOKEN_FILE", global = true)]
    token_file: Option<PathBuf>,
    /// Print the API's own JSON instead of the human form.
    #[arg(long, global = true)]
    json: bool,
    /// A registry spoken to over plain HTTP when a recipe is fetched to check a file, as
    /// `host:port`; repeatable. For a development registry only.
    #[arg(long = "plain-http", global = true)]
    plain_http: Vec<String>,
    #[command(subcommand)]
    command: Command,
    /// The session a `korpis login --no-store` holds in memory for the command it runs.
    #[arg(skip)]
    held: Option<Held>,
}

#[derive(Subcommand)]
enum Command {
    /// Declares the files and shows the real plan, persisted and waiting to be applied; or
    /// approves, rejects, or cancels a plan.
    #[command(args_conflicts_with_subcommands = true)]
    Plan {
        /// A declared file, or a directory of them; repeatable.
        #[arg(short = 'f', long = "file")]
        files: Vec<PathBuf>,
        #[command(flatten)]
        set: SetArgs,
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
        #[command(flatten)]
        set: SetArgs,
    },
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
    /// Shows, sets, and clears an organization's theme in the panel.
    #[command(subcommand)]
    Theme(theme::ThemeAction),
    /// Takes, lists, and shows backups.
    #[command(subcommand)]
    Backup(BackupAction),
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
    Files(korpis_cli::cmd::files::FilesAction),
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

impl Cli {
    /// The connection the flags describe, if they name a server: borne by the token given, or
    /// by the session held or kept, or by nothing.
    async fn connection(&self) -> Result<Option<Connection>, CliError> {
        let Some(server) = &self.server else { return Ok(None) };
        let token = match &self.token_file {
            Some(path) => Some(token_from(path)?),
            None => std::env::var("KORPIS_TOKEN").ok(),
        };
        session::connection(server, self.ca()?, token, self.held.clone()).await.map(Some)
    }

    async fn required(&self) -> Result<Connection, CliError> {
        self.connection().await?.ok_or_else(|| {
            CliError::Local("--server (or KORPIS_SERVER) names the control plane".into())
        })
    }

    /// The installation's CA, which every connection needs.
    fn ca(&self) -> Result<PathBuf, CliError> {
        self.ca.clone().ok_or_else(|| {
            CliError::Local("--ca (or KORPIS_CA) names the installation's CA".into())
        })
    }

    /// A connection to the server named, borne by nothing: what signing in and out begin with.
    fn anonymous(&self) -> Result<Connection, CliError> {
        let server = self.server.clone().ok_or_else(|| {
            CliError::Local("--server (or KORPIS_SERVER) names the control plane".into())
        })?;
        Connection::open(&Endpoint { server, ca: self.ca()?, token: String::new() })
    }
}

/// `korpis login`: the device flow, then the session kept; or, with `--no-store`, `command`
/// run with it held in memory, and the session ended after.
#[expect(clippy::print_stderr, reason = "where to approve the code is said on standard error")]
async fn login(cli: &Cli, no_store: bool, command: &[String]) -> Result<(), CliError> {
    let c = cli.anonymous()?;
    let record = session::device_flow(&c, &mut std::io::stderr().lock()).await?;
    let server = c.server().to_owned();
    if !no_store {
        let vault = Vault::new(&server, Vault::default_dir()?).await;
        eprintln!("{}", session::kept(&vault.save(&record).await?));
        return Ok(());
    }
    let held = Held::Memory(Arc::new(tokio::sync::Mutex::new(record)));
    let ca = cli.ca()?.display().to_string();
    let argv = ["korpis", "--server", &server, "--ca", &ca].into_iter().map(str::to_owned);
    let mut inner = Cli::try_parse_from(argv.chain(command.iter().cloned()))
        .map_err(|e| CliError::Local(e.to_string()))?;
    inner.json = cli.json;
    inner.held = Some(held.clone());
    let ran = Box::pin(run(&inner)).await;
    let ended = session::sign_out(&c, &held).await;
    ran.and(ended)
}

/// `korpis logout`.
async fn logout(cli: &Cli) -> Result<(), CliError> {
    let c = cli.anonymous()?;
    let vault = Vault::new(c.server(), Vault::default_dir()?).await;
    session::sign_out(&c, &Held::Kept(Arc::new(vault))).await
}

#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// What prints each line of a command that answers line by line: the API's object, or the line.
#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
fn printer(json: bool) -> impl FnMut(String, Value) {
    move |text, value| if json { print(&value) } else { println!("{text}") }
}

/// `korpis version`: this binary's, and the server's when there is one to ask.
#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
async fn version(cli: &Cli) -> Result<(), CliError> {
    let v = korpis_cli::version::both(cli.connection().await?.as_ref()).await?;
    if cli.json {
        print(&v);
    } else {
        println!("{}", korpis_cli::version::human(&v));
    }
    Ok(())
}

async fn run(cli: &Cli) -> Result<(), CliError> {
    match &cli.command {
        Command::Completion { shell } => registration(*shell),
        Command::Version => version(cli).await,
        Command::Login { no_store, command } => login(cli, *no_store, command).await,
        Command::Logout => logout(cli).await,
        Command::Plan { action: Some(a), .. } => {
            answer(cli, act::plan(&cli.required().await?, a).await)
        }
        Command::Plan { files, set, action: None } => declared(cli, files, &[], true, set).await,
        Command::Get(g) => answer(cli, get::get(&cli.required().await?, g).await),
        Command::Status { workload } => {
            answer(cli, get::status(&cli.required().await?, workload).await)
        }
        Command::Grant(a) => answer(cli, grant::grant(&cli.required().await?, a).await),
        Command::Set(a) => answer(cli, sets::set(&cli.required().await?, a).await),
        Command::Extension(a) => answer(cli, extension::extension(&cli.required().await?, a).await),
        Command::Event(EventAction::Follow { subscription, ack }) => {
            event::follow(&cli.required().await?, subscription, *ack, &mut printer(cli.json)).await
        }
        Command::Event(a) => answer(cli, event::event(&cli.required().await?, a).await),
        Command::Console(a) => console::console(&cli.required().await?, a).await,
        Command::Files(a) => answer(cli, remote_files(&cli.required().await?, a).await),
        Command::Logs(a) => logs::logs(&cli.required().await?, a, &mut printer(cli.json)).await,
        Command::Stream(a) => answer(cli, stream_share::run(&cli.required().await?, a).await),
        Command::Workload(a) => answer(cli, act::workload(&cli.required().await?, a).await),
        Command::Volume(a) => answer(cli, volume::volume(&cli.required().await?, a).await),
        Command::Repository(a) => {
            answer(cli, repository::repository(&cli.required().await?, a).await)
        }
        Command::Certificate(a) => {
            answer(cli, certificate::certificate(&cli.required().await?, a).await)
        }
        Command::Domain(a) => answer(cli, domain::domain(&cli.required().await?, a).await),
        Command::Theme(a) => answer(cli, theme::theme(&cli.required().await?, a).await),
        Command::Backup(BackupAction::Clone { backup, name, project }) => {
            let c = cli.required().await?;
            answer(cli, Box::pin(clone::clone(&c, backup, (name, project.as_deref()))).await)
        }
        Command::Backup(BackupAction::Cat { backup, path, output }) => {
            backup::cat_to(&cli.required().await?, (backup, path), output.as_deref()).await
        }
        Command::Backup(a) => answer(cli, backup::backup(&cli.required().await?, a).await),
        Command::Node(a) => {
            let server = cli.server.clone().unwrap_or_default();
            answer(cli, act::node(&cli.required().await?, &server, a).await)
        }
        Command::Apply { files, plans, set } => declared(cli, files, plans, false, set).await,
        Command::Call { method, data, list } => {
            call_method(cli, method.as_deref(), data, *list).await
        }
    }
}

/// `korpis call`: `method` with the request `data`, or every method's name for `list`.
#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
async fn call_method(
    cli: &Cli,
    method: Option<&str>,
    data: &str,
    list: bool,
) -> Result<(), CliError> {
    if list {
        for m in METHODS {
            println!("{m}");
        }
        return Ok(());
    }
    let method = method.ok_or_else(|| CliError::Local("name a method, or --list".into()))?;
    let c = cli.required().await?;
    call(&c, method, korpis_cli::call::request(data)?, &mut |v| print(&v)).await
}

/// Prints `a` in the form asked for.
#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
fn answer(cli: &Cli, a: Result<Answer, CliError>) -> Result<(), CliError> {
    let a = a?;
    if cli.json {
        print(&a.json);
    } else {
        print!("{}", a.human);
    }
    Ok(())
}

/// `plan` or `apply` over `paths`.
#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
async fn declared(
    cli: &Cli,
    paths: &[PathBuf],
    ids: &[String],
    propose: bool,
    set: &SetArgs,
) -> Result<(), CliError> {
    let c = cli.required().await?;
    let registry = Registry::client_side(cli.plain_http.clone());
    let out = files::run(&c, &registry, files::Run { paths, ids, propose, set }).await?;
    if cli.json {
        let plans: Vec<&pb::Plan> = out.plans.iter().map(|(_, p)| p).collect();
        print(&serde_json::to_value(plans).unwrap_or_default());
        return Ok(());
    }
    print!("{}", files::human(&out, paths, set));
    Ok(())
}

#[expect(clippy::print_stderr, reason = "a failure is reported on standard error")]
fn main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();
    // Answers a shell's completion request and exits, when the shell is the caller.
    CompleteEnv::with_factory(Cli::command).var(COMPLETE_VAR).complete();
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build();
    let result = match runtime {
        Ok(r) => r.block_on(run(&cli)),
        Err(e) => Err(CliError::Local(format!("the runtime: {e}"))),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if cli.json {
                eprintln!("{}", e.to_json());
            } else {
                eprintln!("korpis: {e}");
            }
            ExitCode::from(e.exit_code())
        }
    }
}
