// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the driver names things in containerd: each container, by a digest of what it was built
//! for, and each signal a workload may be sent, by its number.

use crate::error::{DriverError, DriverResult};

/// The container identifier of `workload` built from `intent`, for `run` when it executes one:
/// one per intent and run, so a create is idempotent, a rebuild never collides with the object
/// it replaces, and two runs never share one. A digest of them, so it is always within
/// containerd's 76 characters and its alphabet, whatever it is given; the labels carry them
/// readably. A persistent workload's object, which executes no run, keeps the digest it had
/// before runs, so an upgraded driver finds it again.
pub(crate) fn container_id(workload: &str, intent: &str, run: &str) -> String {
    let named = if run.is_empty() {
        format!("{workload}\n{intent}")
    } else {
        format!("{workload}\n{intent}\n{run}")
    };
    let digest = korpis_core::content::digest_of(named.as_bytes());
    let hex = digest.trim_start_matches("sha256:");
    format!("korpis-{}", hex.get(..40).unwrap_or(hex))
}

/// Whether `id` is named as [`container_id`] names a container.
pub(crate) fn is_container_id(id: &str) -> bool {
    id.strip_prefix("korpis-").is_some_and(|hex| {
        hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// A driver-declared signal name, as a number.
pub fn signal_number(name: &str) -> DriverResult<u32> {
    Ok(match name {
        "" | "SIGTERM" => 15,
        "SIGKILL" => 9,
        "SIGINT" => 2,
        "SIGHUP" => 1,
        "SIGQUIT" => 3,
        "SIGUSR1" => 10,
        "SIGUSR2" => 12,
        "SIGWINCH" => 28,
        other => return Err(DriverError::Rejected(format!("no signal named {other:?}"))),
    })
}
