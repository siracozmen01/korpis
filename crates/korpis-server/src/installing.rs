// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server install` as a command (§2 of docs/18-operations.md): preflight first, and
//! nothing changed when it refuses; then the users the units run as, the installation in the
//! database, the environment file and the units; and last, what the operator runs next. Where
//! the PostgreSQL server comes from is decided first, in [`provision`].

mod provision;

use clap::Args;
use korpis_app::install::RootHolder;
use korpis_core::tenancy::Name;
use korpis_server::data::DataDir;
use korpis_server::install::preflight::{Asked, control_plane};
use korpis_server::install::target::BackupTarget;
use korpis_server::install::units::{Layout, NODE_STATE, SYSUSERS, units};
use korpis_server::install::{InstallConfig, Installed, install, installed_report, system};
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

/// What `install` is told.
#[derive(Debug, Args)]
pub struct InstallArgs {
    /// The data directory for keys and TLS material.
    #[arg(long, env = "KORPIS_DATA_DIR", default_value = "/var/lib/korpis-server")]
    data_dir: PathBuf,
    /// A database URL with schema-owner rights, for migrations and to make the control plane's
    /// own login, on a server you run.
    #[arg(long, env = "KORPIS_DATABASE_OWNER_URL")]
    database_owner_url: Option<String>,
    #[command(flatten)]
    postgres: provision::PostgresArgs,
    /// The control plane's login, made by the operator and a member of `korpis_app`; unset,
    /// installation makes `korpis_server` with a new password.
    #[arg(long)]
    database_url: Option<String>,
    /// The root organization's name.
    #[arg(long)]
    organization: String,
    /// Names and addresses the server certificate is valid for.
    #[arg(long = "tls-name", default_values = ["localhost", "127.0.0.1"])]
    tls_names: Vec<String>,
    /// Issue the root grant to an API token instead, printed once: a bearer secret with no
    /// second factor, for an unattended install or a test (§9.1 of docs/08-identity.md).
    #[arg(long)]
    root_token: bool,
    /// Where the control plane's own backup goes: a directory on another disk or a network
    /// mount (§6 of docs/18-operations.md). Installation refuses without one.
    #[arg(long)]
    backup_target: Option<BackupTarget>,
    /// The first node's name, whose one-time enrollment is printed; by default this machine's
    /// host name.
    #[arg(long)]
    first_node: Option<String>,
    /// Where the control plane listens.
    #[arg(long, default_value = "[::]:7443")]
    listen: SocketAddr,
    /// The URL clients and nodes reach the control plane at; by default the first TLS name, on
    /// the port listened on.
    #[arg(long)]
    public_url: Option<String>,
    /// The built panel, served on the API's own address.
    #[arg(long)]
    web_root: Option<PathBuf>,
    /// Where the binaries are; by default where this one is.
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// Where the systemd units are written.
    #[arg(long, default_value = "/etc/systemd/system")]
    units_dir: PathBuf,
    /// The environment file the control plane's unit reads, root's alone.
    #[arg(long, default_value = "/etc/korpis/server.env")]
    env_file: PathBuf,
    /// The node agent's state directory.
    #[arg(long, default_value = "/var/lib/korpis-agent")]
    agent_state: PathBuf,
    /// Run preflight and print what would be written, changing nothing.
    #[arg(long)]
    dry_run: bool,
    /// Install into the database and the data directory alone, writing no users, units, or
    /// environment file, for a machine where something else runs the processes: a container,
    /// or a test. A login installation makes is printed, once, since no file keeps it.
    #[arg(long)]
    no_system: bool,
}

/// The sysusers file, read by `systemd-sysusers`.
const SYSUSERS_FILE: &str = "/etc/sysusers.d/korpis.conf";

type Failure = Box<dyn std::error::Error>;

/// Runs the installation `args` describe.
pub async fn run(args: InstallArgs) -> Result<(), Failure> {
    let Some(database) = provision::database(&args).await? else { return Ok(()) };
    let config = config(&args, &database.owner_url)?;
    let facts = korpis_store::server::server_facts(&config.owner_url).await?;
    let report = control_plane(&Asked {
        facts: &facts,
        own_login: args.database_url.is_some(),
        data: config.data.root(),
        target: config.backup_target.as_ref(),
    });
    crate::say(&report.text("This control plane"));
    if !report.offers_anything() {
        return Err("preflight refused the installation; nothing was changed".into());
    }
    if args.dry_run && args.no_system {
        crate::say("\nWould write nothing outside the database and the data directory.\n");
        return Ok(());
    }
    let public_url = public_url(&args);
    if args.no_system {
        let done = install(&config).await?;
        crate::say(&installed_report(&config, &done, &public_url));
        if args.database_url.is_none() {
            crate::say(&format!(
                "\nThe control plane's login is shown once; serve with it:\n\n  \
                 --database-url {}\n",
                done.database_url
            ));
        }
        if let Some(made) = database.made {
            crate::say(&provision::made_report(made, &config.owner_url, None));
        }
        return Ok(());
    }
    on_the_system(&args, &config, &database).await
}

/// The installation with its users, units, and environment files.
async fn on_the_system(
    args: &InstallArgs,
    config: &InstallConfig,
    database: &provision::Database,
) -> Result<(), Failure> {
    let bin = match &args.bin_dir {
        Some(b) => b.clone(),
        None => std::env::current_exe()?.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    if args.dry_run {
        crate::say(&dry_run(args, bin));
        return Ok(());
    }
    if !is_root() {
        return Err("installation writes users and units: run it as root, with --dry-run, or \
                    with --no-system"
            .into());
    }
    system::make_users(Path::new(SYSUSERS_FILE), SYSUSERS)?;
    let agent = lookup("korpis-agent")?;
    let done = install(config).await?;
    let layout = Layout {
        bin,
        data: args.data_dir.clone(),
        agent_state: args.agent_state.clone(),
        env_file: args.env_file.clone(),
        agent_uid: agent.0,
        backup_target: target_dir(args),
    };
    let public_url = public_url(args);
    write_system(args, &done, &public_url, &layout)?;
    crate::say(&installed_report(config, &done, &public_url));
    if let Some(made) = database.made {
        let file = args.env_file.with_file_name("owner.env");
        system::write_environment(
            &file,
            &[("KORPIS_DATABASE_OWNER_URL", config.owner_url.clone())],
        )?;
        crate::say(&provision::made_report(made, &config.owner_url, Some(&file)));
    }
    crate::say(&next_steps());
    Ok(())
}

/// The library's configuration, from the command's.
fn config(args: &InstallArgs, owner_url: &str) -> Result<InstallConfig, Failure> {
    let first_node = match &args.first_node {
        Some(n) => n.clone(),
        None => host_name()?,
    };
    Ok(InstallConfig {
        data: DataDir::new(args.data_dir.clone()),
        owner_url: owner_url.to_owned(),
        organization: Name::parse(args.organization.clone())?,
        tls_names: args.tls_names.clone(),
        root: if args.root_token { RootHolder::Token } else { RootHolder::Passkey },
        first_node: Some(Name::parse(first_node)?),
        backup_target: args.backup_target.clone(),
        database_url: args.database_url.clone(),
    })
}

/// The directory the daily backup writes to; preflight has refused an installation without one.
fn target_dir(args: &InstallArgs) -> PathBuf {
    match &args.backup_target {
        Some(BackupTarget::Directory(dir)) => dir.clone(),
        None => PathBuf::new(),
    }
}

/// This machine's host name, as the kernel holds it.
fn host_name() -> Result<String, Failure> {
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname")?;
    Ok(name.trim().to_owned())
}

fn public_url(args: &InstallArgs) -> String {
    args.public_url.clone().unwrap_or_else(|| {
        let host = args.tls_names.first().map_or("localhost", String::as_str);
        format!("https://{host}:{}", args.listen.port())
    })
}

/// Whether this process runs as root.
fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

fn lookup(name: &str) -> Result<(u32, u32), Failure> {
    system::user(Path::new("/etc/passwd"), name)
        .ok_or_else(|| format!("systemd-sysusers made no user {name}").into())
}

/// The environment file, the units, and the directories handed to their users.
fn write_system(
    args: &InstallArgs,
    done: &Installed,
    public_url: &str,
    layout: &Layout,
) -> Result<(), Failure> {
    let mut env = vec![
        ("KORPIS_DATA_DIR", args.data_dir.display().to_string()),
        ("KORPIS_DATABASE_URL", done.database_url.clone()),
        ("KORPIS_LISTEN", args.listen.to_string()),
        ("KORPIS_PUBLIC_URL", public_url.to_owned()),
    ];
    if let Some(web) = &args.web_root {
        env.push(("KORPIS_WEB_ROOT", web.display().to_string()));
    }
    system::write_environment(&args.env_file, &env)?;
    system::write_units(&args.units_dir, &units(layout))?;
    let korpis = lookup("korpis")?;
    system::give(&args.data_dir, korpis)?;
    // The daily backup runs as the control plane's user, so the directory it writes to is that
    // user's, closed to others.
    let target = target_dir(args);
    std::os::unix::fs::chown(&target, Some(korpis.0), Some(korpis.1))?;
    std::fs::set_permissions(&target, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    std::fs::DirBuilder::new().recursive(true).mode(0o755).create(NODE_STATE)?;
    std::fs::create_dir_all(&args.agent_state)?;
    system::give(&args.agent_state, lookup("korpis-agent")?)?;
    Ok(())
}

/// What `--dry-run` prints after preflight: every file that would be written, and its text.
fn dry_run(args: &InstallArgs, bin: PathBuf) -> String {
    let mut out = format!("\nWould write {SYSUSERS_FILE}:\n\n{SYSUSERS}\n");
    let _ = writeln!(
        out,
        "Would write {} (mode 0600), with the database URL.",
        args.env_file.display()
    );
    let Some((uid, _)) = system::user(Path::new("/etc/passwd"), "korpis-agent") else {
        out.push_str(
            "\nThe units name the uid of korpis-agent, which systemd-sysusers makes on install; \
             run again once it exists to see them.\n",
        );
        return out;
    };
    let layout = Layout {
        bin,
        data: args.data_dir.clone(),
        agent_state: args.agent_state.clone(),
        env_file: args.env_file.clone(),
        agent_uid: uid,
        backup_target: target_dir(args),
    };
    for (name, text) in units(&layout) {
        let _ = write!(out, "\nWould write {}:\n\n{text}", args.units_dir.join(name).display());
    }
    out
}

/// What the operator runs once installation is done, in order.
fn next_steps() -> String {
    "\nThen start the control plane and the node's services, join the node, and start its agent:

  systemctl daemon-reload
  systemctl enable --now korpis-server.service korpis-helper.socket korpis-driver-oci.service
  systemctl enable --now korpis-backup.timer
  (the join command above)
  systemctl enable --now korpis-agent.service

korpis-relay.service is for a machine that is a relay alone; here the control plane is the relay.
"
    .to_owned()
}
