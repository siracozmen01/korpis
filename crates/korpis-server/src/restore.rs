// SPDX-License-Identifier: AGPL-3.0-or-later

//! The machine's side of restoring the control plane (§8 of docs/03-state.md, §6 of
//! docs/18-operations.md), run while it is stopped and before it serves: the backup's keys put
//! back into the data directory, the epochs fenced with the settling window left for the next
//! start to open, the token key rotated, the relays held, and the gap recorded. The database is
//! restored first, by PostgreSQL's own means; step 3, recovering what the agents present, happens
//! once the control plane starts, inside the window.

mod files;

pub use files::{PlaceError, Placed};

use crate::control_backup::{BackupError, Bundle};
use crate::install::{InstallError, RecoverConfig};
use crate::keys::{Keys, stage_token};
use korpis_app::install::PublicKey;
use korpis_app::ports::EnrollmentRead as _;
use korpis_app::ports::SystemClock;
use korpis_app::restore::{BackupEpochs, Fenced, Rotated};
use korpis_app::{App, NoRecipes};
use korpis_core::authz::token::{key_name, parse_key_name};
use korpis_store::PgStore;

/// Rotates the token key: the next epoch's key, past every epoch the data directory or the store
/// holds, staged in the data directory; recorded, with every earlier one retired and every session
/// ended, recorded as asked by `operator`; and only then put in force. A rotation that fails
/// before its transaction commits leaves the data directory signing with the key the store still
/// trusts; run again, it rotates once more.
///
/// # Errors
/// The directory is not an installation's, or the key or the store could not be written.
pub async fn rotate_token_key(
    config: &RecoverConfig,
    operator: &str,
) -> Result<(u32, Rotated), InstallError> {
    if !config.data.tls().join("server.pem").exists() {
        return Err(InstallError::NotInstalled(config.data.root().display().to_string()));
    }
    let keys = Keys::load_or_create(&config.data)?;
    let bodies = std::sync::Arc::new(crate::bodies::ProtoJsonBodies);
    let store = PgStore::connect(&config.database_url, 2, bodies).await?;
    let recorded = store
        .signing_key_ids("token")
        .await
        .map_err(korpis_app::AppError::from)?
        .iter()
        .filter_map(|id| parse_key_name(id))
        .max()
        .unwrap_or(0);
    let staged = stage_token(&config.data, recorded)?;
    let token = PublicKey {
        key_id: key_name(staged.key.key_id),
        purpose: "token",
        public: staged.key.public_bytes(),
    };
    let app = App::new(store, SystemClock, keys.token.clone(), NoRecipes);
    let rotated = app.rotate_token_key(&token, operator).await?;
    Ok((staged.commit()?, rotated))
}

/// What a restore is told.
#[derive(Debug)]
pub struct RestoreConfig {
    /// The installation, and the database already restored.
    pub recover: RecoverConfig,
    /// The control-plane backup file to restore from.
    pub from: std::path::PathBuf,
    /// The key it was sealed with, as installation printed it.
    pub key: zeroize::Zeroizing<[u8; 32]>,
    /// How long the settling window the next start opens lasts.
    pub settle: std::time::Duration,
}

/// What a restore did.
#[derive(Debug)]
pub struct Restored {
    /// When the backup was taken.
    pub taken_at: std::time::SystemTime,
    /// The files put back.
    pub placed: Placed,
    /// Whether the data directory lacked its backup declaration, now written from the file's
    /// directory and the key it was opened with.
    pub declared: bool,
    /// The fence.
    pub fenced: Fenced,
    /// The token key's new epoch, and what rotating it did.
    pub rotated: (u32, Rotated),
    /// The relays held.
    pub relays_held: u64,
}

/// Why a restore failed.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// The backup file could not be read or opened.
    #[error("the backup: {0}")]
    Backup(#[from] BackupError),
    /// Its files could not be put back.
    #[error("the backup's files: {0}")]
    Place(#[from] PlaceError),
    /// The database's schema is not this build's.
    #[error("{0}")]
    Schema(String),
    /// A step failed.
    #[error("{0}")]
    Step(#[from] InstallError),
}

/// Restores the control plane from `config.from` into a data directory and a database already
/// restored, recorded as asked at the machine by `operator`. Each step is safe to run again:
/// a second fence raises nothing the first did not, a second rotation rotates once more, and
/// relays already held stay held; a run stopped part way is finished by running it again.
///
/// # Errors
/// The file, its files, the schema, or a step.
pub async fn restore(config: &RestoreConfig, operator: &str) -> Result<Restored, RestoreError> {
    let bundle = crate::control_backup::open(
        &std::fs::read(&config.from).map_err(BackupError::Io)?,
        &config.key,
    )?;
    let data = &config.recover.data;
    let placed = files::place(data, &bundle.files)?;
    let declared = declare(config)?;
    crate::schema_fits(&config.recover.database_url)
        .await
        .map_err(|e| RestoreError::Schema(e.to_string()))?;
    let taken_at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(bundle.taken_at);
    let fenced = fence(config, &bundle, taken_at, operator).await?;
    let rotated = rotate_token_key(&config.recover, operator).await?;
    let app = app(&config.recover).await?;
    let relays_held = app.hold_relays(operator).await.map_err(InstallError::from)?;
    app.record_restore_gap(taken_at, operator).await.map_err(InstallError::from)?;
    Ok(Restored { taken_at, placed, declared, fenced, rotated, relays_held })
}

/// The application over the restored store, signing with the data directory's token key.
async fn app(config: &RecoverConfig) -> Result<App<PgStore, SystemClock, NoRecipes>, InstallError> {
    let keys = Keys::load_or_create(&config.data)?;
    let bodies = std::sync::Arc::new(crate::bodies::ProtoJsonBodies);
    let store = PgStore::connect(&config.database_url, 2, bodies).await?;
    Ok(App::new(store, SystemClock, keys.token.clone(), NoRecipes))
}

async fn fence(
    config: &RestoreConfig,
    bundle: &Bundle,
    taken_at: std::time::SystemTime,
    operator: &str,
) -> Result<Fenced, InstallError> {
    let count = |what: &str, n: i64| {
        u64::try_from(n).map_err(|_| {
            korpis_app::AppError::Internal(format!("the backup's {what} is negative: {n}"))
        })
    };
    let backup = BackupEpochs {
        max_issued: count("epoch watermark", bundle.epoch.max_issued)?,
        rate_per_sec: count("issue rate", bundle.epoch.issue_rate_per_sec)?,
        taken_at,
    };
    let app = app(&config.recover).await?;
    Ok(app.fence_epochs_at_start(backup, config.settle, operator).await?)
}

/// Writes the data directory's backup declaration if it has none, as a machine restored from
/// nothing does not: the target is the directory the file was read from, and the key the one it
/// was opened with. Whether it wrote it.
fn declare(config: &RestoreConfig) -> Result<bool, RestoreError> {
    use base64::Engine as _;
    let dir = config.recover.data.backup();
    if dir.join("key").exists() || dir.join("target").exists() {
        return Ok(false);
    }
    let target = config.from.parent().unwrap_or_else(|| std::path::Path::new("."));
    let target = std::fs::canonicalize(target).map_err(BackupError::Io)?;
    std::fs::DirBuilder::new().recursive(true).create(&dir).map_err(BackupError::Io)?;
    let key = base64::engine::general_purpose::STANDARD.encode(*config.key);
    crate::data::write_secret(&dir.join("key"), key.as_bytes()).map_err(BackupError::Io)?;
    crate::data::write_secret(&dir.join("target"), target.display().to_string().as_bytes())
        .map_err(BackupError::Io)?;
    Ok(true)
}
