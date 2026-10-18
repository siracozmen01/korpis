// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server restore` (§6 of docs/18-operations.md): run at the machine while the control
//! plane is stopped, after PostgreSQL has restored the database, with the control-plane backup
//! file and the key installation printed for it. It puts the backup's keys back, fences the
//! epochs with the settling window left for the next start, rotates the token key, holds the
//! relays, and records the gap; then the operator starts the control plane, whose first start
//! opens the window in which the agents present what they hold.

use clap::Args;
use korpis_server::data::DataDir;
use korpis_server::install::RecoverConfig;
use korpis_server::restore::{RestoreConfig, Restored, restore};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

/// What `restore` is told.
#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// The installation's data directory, which may be empty: what it lacks of the backup's keys
    /// is put back, and what it holds must be the backup's.
    #[arg(long, env = "KORPIS_DATA_DIR", default_value = "/var/lib/korpis-server")]
    data_dir: PathBuf,
    /// The installation's database, already restored.
    #[arg(long, env = "KORPIS_DATABASE_URL")]
    database_url: String,
    /// The control-plane backup file to restore from, one `korpis-server backup` wrote.
    #[arg(long)]
    from: PathBuf,
    /// The key the backup was sealed with, base64, as installation printed it; unset, read from
    /// --key-file.
    #[arg(long, env = "KORPIS_BACKUP_KEY", hide_env_values = true)]
    key: Option<String>,
    /// A file holding the key.
    #[arg(long)]
    key_file: Option<PathBuf>,
    /// How long the settling window lasts, in seconds, from the control plane's next start:
    /// long enough for every agent to reconnect and present what it holds.
    #[arg(long, default_value_t = 600)]
    settle_secs: u64,
}

type Failure = Box<dyn std::error::Error>;

/// The 32-byte key, from the flag or the file.
fn key(args: &RestoreArgs) -> Result<zeroize::Zeroizing<[u8; 32]>, Failure> {
    use base64::Engine as _;
    let encoded = match (&args.key, &args.key_file) {
        (Some(k), _) => zeroize::Zeroizing::new(k.clone()),
        (None, Some(path)) => zeroize::Zeroizing::new(
            std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?,
        ),
        (None, None) => return Err("name the backup's key with --key or --key-file".into()),
    };
    let bytes = zeroize::Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| "the backup's key is not base64")?,
    );
    let key: [u8; 32] =
        bytes.as_slice().try_into().map_err(|_| "the backup's key is not 32 bytes")?;
    Ok(zeroize::Zeroizing::new(key))
}

/// Restores as `args` say, recorded as asked by `operator`.
pub async fn run(args: RestoreArgs, operator: &str) -> Result<(), Failure> {
    let config = RestoreConfig {
        key: key(&args)?,
        recover: RecoverConfig {
            data: DataDir::new(args.data_dir),
            database_url: args.database_url,
        },
        from: args.from,
        settle: Duration::from_secs(args.settle_secs),
    };
    let restored = restore(&config, operator).await?;
    crate::say(&report(&restored, config.settle));
    Ok(())
}

fn report(r: &Restored, settle: Duration) -> String {
    let taken = r.taken_at.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut out = format!(
        "Restored from the backup taken at {taken} (seconds since the Unix epoch).\n\
         Keys: {} put back, {} already here.\n",
        r.placed.written, r.placed.kept
    );
    if r.declared {
        out.push_str("The backup declaration was missing: the file's directory and its key are now declared.\n");
    }
    let (epoch, rotated) = r.rotated;
    let _ = writeln!(
        out,
        "Epochs: nothing at or below {} is issued again.\n\
         Token key: now epoch {epoch}; {} retired, {} sessions ended.\n\
         Relays held until readmitted or removed: {}.\n\
         The gap between the backup and now is recorded.\n\
         Start the control plane: for {} seconds from its start, agents present what they hold \
         and nothing else is served.",
        r.fenced.floor,
        rotated.retired,
        rotated.sessions_ended,
        r.relays_held,
        settle.as_secs()
    );
    out
}
