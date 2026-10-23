// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis`: the reference client.

use clap::{CommandFactory as _, Parser};
use clap_complete::CompleteEnv;
use korpis_cli::call::{METHODS, call};
use korpis_cli::cmd::backup::{self, BackupAction};
use korpis_cli::cmd::certificate;
use korpis_cli::cmd::clone;
use korpis_cli::cmd::domain;
use korpis_cli::cmd::event::{self, EventAction};
use korpis_cli::cmd::extension;
use korpis_cli::cmd::files::files as remote_files;
use korpis_cli::cmd::identity;
use korpis_cli::cmd::portal;
use korpis_cli::cmd::quota;
use korpis_cli::cmd::repository;
use korpis_cli::cmd::run::{self as runs, RunAction};
use korpis_cli::cmd::template;
use korpis_cli::cmd::volume;
use korpis_cli::cmd::{Answer, act, get, grant, sets};
use korpis_cli::cmd::{console, layout, logs, sftp, ssh_key, stream_share, tenancy, theme};
use korpis_cli::command::Command;
use korpis_cli::complete::{COMPLETE_VAR, registration};
use korpis_cli::connect::{Connection, Endpoint, token_from};
use korpis_cli::error::CliError;
use korpis_cli::files;
use korpis_cli::inside;
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
    /// Inside a workload, its identity socket, whose credential bears the calls; by default
    /// `/run/korpis/identity.sock` when it exists and no server is named.
    #[arg(long, env = "KORPIS_IDENTITY", global = true)]
    identity: Option<PathBuf>,
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

impl Cli {
    /// The connection the flags describe: inside a workload, borne by its credential; to a
    /// server named, by the token given, or by the session held or kept, or by nothing.
    async fn connection(&self) -> Result<Option<Connection>, CliError> {
        if let Some(socket) = inside::socket(self.identity.clone(), self.server.is_some()) {
            return inside::connection(&socket).await.map(Some);
        }
        let Some(server) = &self.server else { return Ok(None) };
        let token = match &self.token_file {
            Some(path) => Some(token_from(path)?),
            None => std::env::var("KORPIS_TOKEN").ok(),
        };
        session::connection(server, self.ca()?, token, self.held.clone()).await.map(Some)
    }

    async fn required(&self) -> Result<Connection, CliError> {
        self.connection().await?.ok_or_else(no_server)
    }

    /// The installation's CA, which every connection needs.
    fn ca(&self) -> Result<PathBuf, CliError> {
        self.ca.clone().ok_or_else(|| {
            CliError::Local("--ca (or KORPIS_CA) names the installation's CA".into())
        })
    }

    /// A connection to the server named, borne by nothing: what signing in and out begin with.
    fn anonymous(&self) -> Result<Connection, CliError> {
        let server = self.server.clone().ok_or_else(no_server)?;
        Connection::open(&Endpoint { server, ca: self.ca()?, token: String::new() })
    }
}

fn no_server() -> CliError {
    CliError::Local("--server (or KORPIS_SERVER) names the control plane".into())
}

/// `korpis ssh-key …`; a change refused as not recent signs the terminal in again by the device
/// flow and is asked once more (Finding 135 of docs/23-walkthroughs.md).
#[expect(clippy::print_stderr, reason = "where to approve the code is said on standard error")]
async fn ssh_keys(cli: &Cli, a: &ssh_key::SshKeyAction) -> Result<Answer, CliError> {
    let first = ssh_key::ssh_key(&cli.required().await?, a).await;
    match first {
        Err(e) if a.changes() && ssh_key::not_recent(&e) => {
            eprintln!("This needs a sign-in within ten minutes; approve this terminal again.");
            let c = cli.anonymous()?;
            let held = match cli.held.clone() {
                Some(held) => held,
                None => Held::Kept(Arc::new(Vault::new(c.server(), Vault::default_dir()?).await)),
            };
            session::signed_in_again(&c, &held, &mut std::io::stderr().lock()).await?;
            ssh_key::ssh_key(&cli.required().await?, a).await
        }
        other => other,
    }
}

/// `korpis heartbeat`: through the identity socket named, or the one mounted into the workload.
async fn heartbeat(cli: &Cli, every: Option<&str>) -> Result<(), CliError> {
    let socket = inside::socket(cli.identity.clone(), false).ok_or_else(|| {
        CliError::Local("no identity socket: run it inside a workload, or name one".into())
    })?;
    let every = every.map(korpis_cli::cmd::seconds).transpose()?;
    let every = every.and_then(|s| u64::try_from(s).ok()).map(std::time::Duration::from_secs);
    inside::heartbeat(&socket, every).await
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

#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// What prints each line of a command that answers line by line: the API's object, or the line.
#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
fn printer(json: bool) -> impl FnMut(String, Value) {
    move |text, value| if json { print(&value) } else { println!("{text}") }
}

async fn run(cli: &Cli) -> Result<(), CliError> {
    match &cli.command {
        Command::Completion { shell } => registration(*shell),
        Command::Version => {
            answer(cli, korpis_cli::version::answer(cli.connection().await?.as_ref()).await)
        }
        Command::Login { no_store, command } => login(cli, *no_store, command).await,
        Command::Logout => session::log_out(&cli.anonymous()?).await,
        Command::Heartbeat { every } => heartbeat(cli, every.as_deref()).await,
        Command::Plan { action: Some(a), .. } => {
            answer(cli, act::plan(&cli.required().await?, a).await)
        }
        Command::Plan { files, set, action: None } => declared(cli, files, &[], true, set).await,
        Command::Get(g) => answer(cli, get::get(&cli.required().await?, g).await),
        Command::Organization(_)
        | Command::Project(_)
        | Command::Quota(_)
        | Command::Template(_)
        | Command::Identity(_)
        | Command::Grant(_)
        | Command::Set(_)
        | Command::Extension(_)
        | Command::Volume(_)
        | Command::Repository(_)
        | Command::Certificate(_)
        | Command::Domain(_)
        | Command::Portal(_)
        | Command::Theme(_)
        | Command::Layout(_) => answer(cli, object(&cli.required().await?, &cli.command).await),
        Command::Status { workload } => {
            answer(cli, get::status(&cli.required().await?, workload).await)
        }
        Command::Event(EventAction::Follow { subscription, ack }) => {
            event::follow(&cli.required().await?, subscription, *ack, &mut printer(cli.json)).await
        }
        Command::SshKey(a) => answer(cli, Box::pin(ssh_keys(cli, a)).await),
        Command::Event(a) => answer(cli, event::event(&cli.required().await?, a).await),
        Command::Console(a) => console::console(&cli.required().await?, a).await,
        Command::Files(a) => answer(cli, remote_files(&cli.required().await?, a).await),
        Command::Sftp { workload } => {
            answer(cli, sftp::sftp(&cli.required().await?, workload).await)
        }
        Command::Logs(a) => logs::logs(&cli.required().await?, a, &mut printer(cli.json)).await,
        Command::Stream(a) => answer(cli, stream_share::run(&cli.required().await?, a).await),
        Command::Workload(a) => answer(cli, act::workload(&cli.required().await?, a).await),
        Command::Backup(a) => backups(cli, a).await,
        Command::Run(RunAction::Logs { run, reading }) => {
            runs::logs(&cli.required().await?, run, reading, &mut printer(cli.json)).await
        }
        Command::Run(a) => answer(cli, runs::run(&cli.required().await?, a).await),
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

/// `korpis backup …`: a clone and a file's contents write as they go; the rest answer.
async fn backups(cli: &Cli, action: &BackupAction) -> Result<(), CliError> {
    match action {
        BackupAction::Clone { backup, name, project } => {
            let c = cli.required().await?;
            answer(cli, Box::pin(clone::clone(&c, backup, (name, project.as_deref()))).await)
        }
        BackupAction::Cat { backup, path, output } => {
            backup::cat_to(&cli.required().await?, (backup, path), output.as_deref()).await
        }
        a => answer(cli, backup::backup(&cli.required().await?, a).await),
    }
}

/// The commands that make, change, or show one kind of object, and answer with it.
async fn object(c: &Connection, command: &Command) -> Result<Answer, CliError> {
    match command {
        Command::Organization(a) => tenancy::organization(c, a).await,
        Command::Project(a) => tenancy::project(c, a).await,
        Command::Quota(a) => quota::quota(c, a).await,
        Command::Template(a) => template::template(c, a).await,
        Command::Identity(a) => identity::identity(c, a).await,
        Command::Grant(a) => grant::grant(c, a).await,
        Command::Set(a) => sets::set(c, a).await,
        Command::Extension(a) => extension::extension(c, a).await,
        Command::Volume(a) => volume::volume(c, a).await,
        Command::Repository(a) => repository::repository(c, a).await,
        Command::Certificate(a) => certificate::certificate(c, a).await,
        Command::Domain(a) => domain::domain(c, a).await,
        Command::Portal(a) => portal::portal(c, a).await,
        Command::Theme(a) => theme::theme(c, a).await,
        Command::Layout(a) => layout::layout(c, a).await,
        _ => Err(CliError::Local("not a command about one kind of object".into())),
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
