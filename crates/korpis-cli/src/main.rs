// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis`: the reference client.

use clap::{Parser, Subcommand};
use korpis_cli::apply::{Mode, apply_planned, declare, plans};
use korpis_cli::call::{METHODS, call};
use korpis_cli::cmd::{
    Answer, Get, GrantAction, NodeAction, PlanAction, WorkloadAction, act, get, grant,
};
use korpis_cli::connect::{Connection, Endpoint, token_from};
use korpis_cli::declared::read_all;
use korpis_cli::error::CliError;
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
    #[arg(long, global = true)]
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
    },
    /// Lists what you can read.
    #[command(subcommand)]
    Get(Get),
    /// A workload in detail: where it runs, what it is doing, and what it waits for.
    Status {
        /// The workload: an identifier, `project/name`, or a name.
        workload: String,
    },
    /// Starts, stops, restarts, or deletes a workload.
    #[command(subcommand)]
    Workload(WorkloadAction),
    /// Enrolls, cordons, or uncordons a node.
    #[command(subcommand)]
    Node(NodeAction),
    /// Issues, revokes, lists, and shows grants.
    #[command(subcommand)]
    Grant(GrantAction),
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

#[expect(clippy::print_stdout, reason = "a command's answer is the binary's output")]
async fn run(cli: &Cli) -> Result<(), CliError> {
    match &cli.command {
        Command::Version => {
            let v = korpis_cli::version::both(cli.connection()?.as_ref()).await?;
            if cli.json {
                print(&v);
            } else {
                println!("{}", korpis_cli::version::human(&v));
            }
            Ok(())
        }
        Command::Plan { action: Some(a), .. } => answer(cli, act::plan(&cli.required()?, a).await),
        Command::Plan { files, action: None } => declared(cli, files, &[], true).await,
        Command::Get(g) => answer(cli, get::get(&cli.required()?, g).await),
        Command::Status { workload } => answer(cli, get::status(&cli.required()?, workload).await),
        Command::Grant(a) => answer(cli, grant::grant(&cli.required()?, a).await),
        Command::Workload(a) => answer(cli, act::workload(&cli.required()?, a).await),
        Command::Node(a) => {
            let server = cli.server.clone().unwrap_or_default();
            answer(cli, act::node(&cli.required()?, &server, a).await)
        }
        Command::Apply { files, plans } => declared(cli, files, plans, false).await,
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
) -> Result<(), CliError> {
    let c = cli.required()?;
    let registry = Registry::new(cli.plain_http.clone());
    let files = read_all(paths)?;
    let (mut out, mut waiting) = (Vec::new(), Vec::new());
    if ids.is_empty() {
        let mode = if propose { Mode::Propose } else { Mode::Apply };
        for f in &files {
            let d = declare(&c, &registry, f, mode).await?;
            let p = d.plan.as_option().cloned().unwrap_or_default();
            if p.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
                waiting.push(p.id.clone());
            }
            out.push((f.declared.workload.clone(), p));
        }
    } else {
        let plans = plans(&c, ids).await?;
        for f in &files {
            out.push((f.declared.workload.clone(), apply_planned(&c, &registry, f, &plans).await?));
        }
    }
    if cli.json {
        let plans: Vec<&pb::Plan> = out.iter().map(|(_, p)| p).collect();
        print(&serde_json::to_value(plans).unwrap_or_default());
        return Ok(());
    }
    for (workload, p) in &out {
        println!("{}", show::plan(p, workload));
    }
    if !waiting.is_empty() {
        let shown: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        println!("{}", show::apply_with(&shown, &waiting));
    }
    Ok(())
}

#[expect(clippy::print_stderr, reason = "a failure is reported on standard error")]
#[tokio::main]
async fn main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();
    match run(&cli).await {
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
