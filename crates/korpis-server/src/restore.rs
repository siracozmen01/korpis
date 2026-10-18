// SPDX-License-Identifier: AGPL-3.0-or-later

//! The machine's side of restoring the control plane (§8 of docs/03-state.md, §6 of
//! docs/18-operations.md), run before the restored control plane serves: what needs the data
//! directory's private keys, which the store never holds. The restore command that runs every
//! step in order is built with the last of them.

use crate::install::{InstallError, RecoverConfig};
use crate::keys::{Keys, stage_token};
use korpis_app::install::PublicKey;
use korpis_app::ports::EnrollmentRead as _;
use korpis_app::ports::SystemClock;
use korpis_app::restore::Rotated;
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
