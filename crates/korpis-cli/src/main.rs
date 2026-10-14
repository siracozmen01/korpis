// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis`: the reference client.

use clap::{CommandFactory as _, Parser, Subcommand};
use clap_complete::CompleteEnv;
use clap_complete::engine::ArgValueCompleter;
use korpis_cli::call::{METHODS, call};
use korpis_cli::cmd::backup::{self, BackupAction};
use korpis_cli::cmd::clone;
use korpis_cli::cmd::event::{self, EventAction};
use korpis_cli::cmd::extension::{self, ExtensionAction};
use korpis_cli::cmd::repository::{self, RepositoryAction};
use korpis_cli::cmd::volume::{self, VolumeAction};
use korpis_cli::cmd::{
    Answer, Get, GrantAction, NodeAction, PlanAction, SetAction, WorkloadAction, act, get, grant,
    sets,
};
use korpis_cli::cmd::{console, logs};
use korpis_cli::complete;
use korpis_cli::connect::{Connection, Endpoint, token_from};
use korpis_cli::error::CliError;
use korpis_cli::files;
use korpis_cli::sets::SetArgs;
use korpis_cli::show;
use korpis_proto::korpis::v1 as pb;
use korpis_recipe::registry::Registry;
use serde_json::Value;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;

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
    /// The connection the flags describe, if they name a server.
    fn connection(&self) -> Result<Option<Connection>, CliError> {
        let Some(server) = &self.server else { return Ok(None) };
        let ca = self.ca.clone().ok_or_else(|| {
            CliError::Local("--ca (or KORPIS_CA) names the installation's CA".into())
        })?;
        let token = match &self.token_file {
            Some(path) => token_from(path)?,
            None => std::env::var("KORPIS_TOKEN").unwrap_or_default(),
        };
        Connection::open(&Endpoint { server: server.clone(), ca, token }).map(Some)
    }

    fn required(&self) -> Result<Connection, CliError> {
        self.connection()?.ok_or_else(|| {
            CliError::Local("--server (or KORPIS_SERVER) names the control plane".into())
        })
    }
}

/// The request `data` names.
fn request(data: &str) -> Result<Value, CliError> {
    let text = match data {
        "-" => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).map_err(|e| CliError::Local(e.to_string()))?;
            s
        }
        _ if data.starts_with('@') => std::fs::read_to_string(&data[1..])
            .map_err(|e| CliError::Local(format!("{}: {e}", &data[1..])))?,
        _ => data.to_owned(),
    };
    serde_json::from_str(&text).map_err(|e| CliError::Local(format!("the request: {e}")))
}

#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// `korpis backup cat`: the file's bytes to `output`, made new, or to standard output.
async fn cat(
    c: &korpis_cli::connect::Connection,
    file: (&str, &str),
    output: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let local = |e: std::io::Error| CliError::Local(e.to_string());
    match output {
        Some(path) => {
            let mut f = std::fs::File::create_new(path).map_err(local)?;
            backup::cat(c, file, &mut f).await.map(|_| ())
        }
        None => backup::cat(c, file, &mut std::io::stdout().lock()).await.map(|_| ()),
    }
}

/// `korpis version`: this binary's, and the server's when there is one to ask.
#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
async fn version(cli: &Cli) -> Result<(), CliError> {
    let v = korpis_cli::version::both(cli.connection()?.as_ref()).await?;
    if cli.json {
        print(&v);
    } else {
        println!("{}", korpis_cli::version::human(&v));
    }
    Ok(())
}

#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
async fn run(cli: &Cli) -> Result<(), CliError> {
    match &cli.command {
        Command::Completion { shell } => registration(*shell),
        Command::Version => version(cli).await,
        Command::Plan { action: Some(a), .. } => answer(cli, act::plan(&cli.required()?, a).await),
        Command::Plan { files, set, action: None } => declared(cli, files, &[], true, set).await,
        Command::Get(g) => answer(cli, get::get(&cli.required()?, g).await),
        Command::Status { workload } => answer(cli, get::status(&cli.required()?, workload).await),
        Command::Grant(a) => answer(cli, grant::grant(&cli.required()?, a).await),
        Command::Set(a) => answer(cli, sets::set(&cli.required()?, a).await),
        Command::Extension(a) => answer(cli, extension::extension(&cli.required()?, a).await),
        Command::Event(EventAction::Follow { subscription, ack }) => {
            let c = cli.required()?;
            let json = cli.json;
            event::follow(&c, subscription, *ack, &mut |text, value| {
                if json { print(&value) } else { println!("{text}") }
            })
            .await
        }
        Command::Event(a) => answer(cli, event::event(&cli.required()?, a).await),
        Command::Console(a) => console::console(&cli.required()?, a).await,
        Command::Logs(a) => {
            let c = cli.required()?;
            let json = cli.json;
            logs::logs(&c, a, &mut |text, value| {
                if json { print(&value) } else { println!("{text}") }
            })
            .await
        }
        Command::Workload(a) => answer(cli, act::workload(&cli.required()?, a).await),
        Command::Volume(a) => answer(cli, volume::volume(&cli.required()?, a).await),
        Command::Repository(a) => answer(cli, repository::repository(&cli.required()?, a).await),
        Command::Backup(BackupAction::Clone { backup, name, project }) => {
            let c = cli.required()?;
            answer(cli, Box::pin(clone::clone(&c, backup, (name, project.as_deref()))).await)
        }
        Command::Backup(BackupAction::Cat { backup, path, output }) => {
            cat(&cli.required()?, (backup, path), output.as_deref()).await
        }
        Command::Backup(a) => answer(cli, backup::backup(&cli.required()?, a).await),
        Command::Node(a) => {
            let server = cli.server.clone().unwrap_or_default();
            answer(cli, act::node(&cli.required()?, &server, a).await)
        }
        Command::Apply { files, plans, set } => declared(cli, files, plans, false, set).await,
        Command::Call { list: true, .. } => {
            for m in METHODS {
                println!("{m}");
            }
            Ok(())
        }
        Command::Call { method: None, .. } => {
            Err(CliError::Local("name a method, or --list".into()))
        }
        Command::Call { method: Some(method), data, .. } => {
            let c = cli.required()?;
            call(&c, method, request(data)?, &mut |v| print(&v)).await
        }
    }
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
    let c = cli.required()?;
    let registry = Registry::new(cli.plain_http.clone());
    let out = files::run(&c, &registry, files::Run { paths, ids, propose, set }).await?;
    if cli.json {
        let plans: Vec<&pb::Plan> = out.plans.iter().map(|(_, p)| p).collect();
        print(&serde_json::to_value(plans).unwrap_or_default());
        return Ok(());
    }
    for (workload, p) in &out.plans {
        println!("{}", show::plan(p, workload));
    }
    for w in &out.left {
        println!("left as it is: {w} drifted, and --accept-drift was given");
    }
    if !out.waiting.is_empty() {
        let shown: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        println!("{}", show::apply_with(&shown, &out.waiting, &set_flags(set)));
    }
    Ok(())
}

/// The set flags `apply` must be given again to apply what `plan` showed.
fn set_flags(set: &SetArgs) -> String {
    let mut flags = Vec::new();
    if let Some(name) = &set.set {
        flags.push(format!("--set {name}"));
    }
    for (on, flag) in [(set.adopt, "--adopt"), (set.reconcile, "--reconcile")] {
        if on {
            flags.push(flag.to_owned());
        }
    }
    flags.join(" ")
}

/// The shells completion can be registered with.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Shell {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Powershell,
}

/// The environment variable that asks the binary for completions.
const COMPLETE_VAR: &str = "KORPIS_COMPLETE";

/// Writes the registration script for `shell`, pointing at this binary.
fn registration(shell: Shell) -> Result<(), CliError> {
    use clap_complete::env::{Bash, Elvish, EnvCompleter, Fish, Powershell, Zsh};
    let completer: &dyn EnvCompleter = match shell {
        Shell::Bash => &Bash,
        Shell::Zsh => &Zsh,
        Shell::Fish => &Fish,
        Shell::Elvish => &Elvish,
        Shell::Powershell => &Powershell,
    };
    let bin = std::env::current_exe().map_err(|e| CliError::Local(e.to_string()))?;
    let bin = bin.to_string_lossy();
    let mut out = std::io::stdout().lock();
    completer
        .write_registration(COMPLETE_VAR, "korpis", &bin, &bin, &mut out)
        .map_err(|e| CliError::Local(e.to_string()))
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
