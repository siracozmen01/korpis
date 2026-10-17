// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where the PostgreSQL server comes from, decided before anything else (§2 of
//! docs/18-operations.md): named by `--postgres`, implied by `--database-owner-url`, or asked on
//! a terminal. A server the installer provisions is provisioned only after what preflight can
//! judge without it, the data directory and the backup target, has passed, so a refusal there
//! still changes nothing.

use super::{Failure, InstallArgs, is_root};
use clap::Args;
use korpis_core::preflight::Report;
use korpis_server::install::postgres::container::{self, Container, Engine};
use korpis_server::install::postgres::package::{self, Manager};
use korpis_server::install::postgres::{self, Source};
use korpis_server::install::{preflight, system, target};
use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::Path;

/// Where the server comes from, and how a provisioned one is made.
#[derive(Debug, Args)]
pub struct PostgresArgs {
    /// Where PostgreSQL comes from: `existing`, named by --database-owner-url; `package`, this
    /// distribution's, by its package manager; or `container`, the official image by podman or
    /// docker, for evaluation. Asked on a terminal when neither this nor a URL is given.
    #[arg(long)]
    postgres: Option<Source>,
    /// The container's name, for `--postgres container`; its volume is named after it.
    #[arg(long, default_value = "korpis-postgres")]
    postgres_container: String,
    /// The loopback port the container is published on.
    #[arg(long, default_value_t = 5432)]
    postgres_port: u16,
    /// The image the container runs, by digest.
    #[arg(long, default_value = container::IMAGE)]
    postgres_image: String,
}

/// The server the installation goes into: the owner's URL, and what the installer made, if
/// anything.
#[derive(Debug)]
pub struct Database {
    /// Connects with schema-owner rights.
    pub owner_url: String,
    /// What the installer provisioned; none for a server the operator named.
    pub made: Option<Source>,
}

/// The server: named, or provisioned now. None when a dry run has said what it would do.
pub async fn database(args: &InstallArgs) -> Result<Option<Database>, Failure> {
    let source = source(args)?;
    if source == Source::Existing {
        let owner_url = args.database_owner_url.clone().ok_or(
            "a server you run is named with --database-owner-url, a URL with schema-owner rights",
        )?;
        return Ok(Some(Database { owner_url, made: None }));
    }
    let machine = Report(vec![
        preflight::data_directory(&args.data_dir),
        target::check(args.backup_target.as_ref(), &args.data_dir),
    ]);
    if !machine.offers_anything() {
        crate::say(&machine.text("This control plane"));
        return Err("preflight refused the installation; nothing was changed".into());
    }
    let owner_url = match source {
        Source::Package => package(args)?,
        _ => container(args).await?,
    };
    Ok(owner_url.map(|owner_url| Database { owner_url, made: Some(source) }))
}

fn source(args: &InstallArgs) -> Result<Source, Failure> {
    if let Some(s) = args.postgres.postgres {
        return Ok(s);
    }
    if args.database_owner_url.is_some() {
        return Ok(Source::Existing);
    }
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        return Ok(postgres::ask(&mut std::io::stdin().lock(), &mut std::io::stderr())?);
    }
    Err("name the server with --database-owner-url, or have one made with --postgres package or \
         --postgres container"
        .into())
}

/// Installs the distribution's server; its owner's URL, or none on a dry run.
fn package(args: &InstallArgs) -> Result<Option<String>, Failure> {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let manager: Manager = package::detect(&os_release).ok_or(
        "this distribution's package manager is neither apt nor dnf: name a server you run",
    )?;
    if args.dry_run {
        let mut out = String::from("\nWould install PostgreSQL by running:\n\n");
        for step in package::steps(manager) {
            let _ = writeln!(out, "  {step}");
        }
        out.push_str("\nand make the owner korpis_owner and the database korpis as postgres.\n");
        crate::say(&out);
        return Ok(None);
    }
    if args.no_system || !is_root() {
        return Err("--postgres package installs a system service: run it as root, without \
                    --no-system"
            .into());
    }
    Ok(Some(package::provision(manager)?))
}

/// Runs the official image; its owner's URL, or none on a dry run.
async fn container(args: &InstallArgs) -> Result<Option<String>, Failure> {
    let p = &args.postgres;
    if !p.postgres_image.contains("@sha256:") {
        return Err("--postgres-image names an image by digest: <name>@sha256:<digest>".into());
    }
    let engine = Engine::found().ok_or("neither podman nor docker is on PATH")?;
    let c = Container {
        engine,
        name: p.postgres_container.clone(),
        image: p.postgres_image.clone(),
        port: p.postgres_port,
    };
    let unit = if args.no_system { None } else { c.unit() };
    if args.dry_run {
        let made = c.make().join(" ");
        let mut out = format!("\nWould run PostgreSQL in a container:\n\n  {made}\n");
        if let Some((name, text)) = &unit {
            let _ = write!(
                out,
                "
and write {}:

{text}",
                args.units_dir.join(name).display()
            );
        }
        crate::say(&out);
        return Ok(None);
    }
    let superuser = container::make(&c)?;
    match &unit {
        Some((name, text)) => {
            system::write_units(&args.units_dir, &[(name.as_str(), text.clone())])?;
            postgres::run(&["systemctl", "daemon-reload"], &[], None)?;
            postgres::run(&["systemctl", "enable", "--now", name], &[], None)?;
        }
        None if engine == Engine::Podman => container::start(&c)?,
        None => {}
    }
    Ok(Some(container::finish(&c, &superuser).await?))
}

/// What the installer made, said after the installation: where the owner's URL is kept, or the
/// URL itself when no file keeps it; and, for a container, what it is not.
pub fn made_report(made: Source, owner_url: &str, owner_file: Option<&Path>) -> String {
    let mut out = String::new();
    match owner_file {
        Some(file) => {
            let _ = write!(
                out,
                "\nThe owner's URL, which an upgrade migrates with, is in {}, root's alone.\n",
                file.display()
            );
        }
        None => {
            let _ = write!(
                out,
                "\nThe owner's URL, which an upgrade migrates with, is shown once:\n\n  {owner_url}\n"
            );
        }
    }
    if made == Source::Container {
        out.push_str(
            "\nPostgreSQL runs in a container: an evaluation setup, not a production one. Nothing \
             backs it up, upgrades it, or watches it but you (§2 of docs/18-operations.md).\n",
        );
    }
    out
}
