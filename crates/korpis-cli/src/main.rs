// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis`: the reference client.

use clap::{Parser, Subcommand};
use korpis_cli::call::{METHODS, call};
use korpis_cli::connect::{Connection, Endpoint, token_from};
use korpis_cli::error::CliError;
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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
