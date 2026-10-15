// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider's configuration, its workload's environment. Every variable has a default, so a
//! recipe that names none serves its volume at `/data` on port 9000.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Every variable, with what it holds.
pub const VARIABLES: &[(&str, &str)] = &[
    ("KORPIS_FS_ROOT", "where the objects are kept: the provider's volume (default /data)"),
    ("KORPIS_FS_LISTEN", "where the store is served (default 0.0.0.0:9000)"),
    ("KORPIS_FS_ENDPOINT", "the name of the endpoint it is served on (default store)"),
    ("KORPIS_FS_NAME", "the name it provides as (default fs)"),
];

/// The configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Where the objects are kept.
    pub root: PathBuf,
    /// Where the store is served.
    pub listen: SocketAddr,
    /// The endpoint it is served on, by name.
    pub endpoint: String,
    /// The name it provides as.
    pub name: String,
}

/// Why the environment configures nothing.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(String);

/// The configuration `env` gives.
///
/// # Errors
/// A variable holds what it cannot.
pub fn from<S: std::hash::BuildHasher>(
    env: &HashMap<String, String, S>,
) -> Result<Config, ConfigError> {
    let get = |k: &str, default: &str| env.get(k).cloned().unwrap_or_else(|| default.to_owned());
    let listen = get("KORPIS_FS_LISTEN", "0.0.0.0:9000");
    let listen =
        listen.parse().map_err(|_| ConfigError(format!("KORPIS_FS_LISTEN: {listen:?}")))?;
    Ok(Config {
        root: PathBuf::from(get("KORPIS_FS_ROOT", "/data")),
        listen,
        endpoint: get("KORPIS_FS_ENDPOINT", "store"),
        name: get("KORPIS_FS_NAME", "fs"),
    })
}
