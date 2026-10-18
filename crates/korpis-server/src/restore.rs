// SPDX-License-Identifier: AGPL-3.0-or-later

//! The machine's side of restoring the control plane (§8 of docs/03-state.md, §6 of
//! docs/18-operations.md), run before the restored control plane serves: what needs the data
//! directory's private keys, which the store never holds. The restore command that runs every
//! step in order is built with the last of them.

use crate::install::{InstallError, RecoverConfig};
use crate::keys::{Keys, rotate_token};
use korpis_app::ports::SystemClock;
use korpis_app::restore::Rotated;
use korpis_app::{App, NoRecipes};
use korpis_store::PgStore;

/// Rotates the token key: the next epoch's key made in the data directory, recorded, and every
/// earlier one retired, with every session ended, recorded as asked by `operator`. Run again after
/// a failure, it rotates once more, which voids nothing it should not.
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
    let epoch = rotate_token(&config.data)?;
    let keys = Keys::load_or_create(&config.data)?;
    let bodies = std::sync::Arc::new(crate::bodies::ProtoJsonBodies);
    let store = PgStore::connect(&config.database_url, 2, bodies).await?;
    let app = App::new(store, SystemClock, keys.token.clone(), NoRecipes);
    let token = keys
        .public()
        .into_iter()
        .find(|k| k.purpose == "token")
        .ok_or_else(|| InstallError::Data(std::io::Error::other("no token key was loaded")))?;
    Ok((epoch, app.rotate_token_key(&token, operator).await?))
}
