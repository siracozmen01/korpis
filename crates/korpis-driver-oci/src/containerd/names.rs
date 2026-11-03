// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the driver names things in containerd: each container, by a digest of what it was built
//! for, and each signal a workload may be sent, by its number.

use crate::error::{DriverError, DriverResult};

/// The container identifier of `workload` built from `intent`, for `run` when it executes one,
/// kept by `owner`: one per intent and run, so a create is idempotent, a rebuild never collides
/// with the object it replaces, and two runs never share one; and one per owner, so two nodes
/// sharing the namespace never name a workload's object alike, as the source and destination of a
/// migration would. A digest of them, so it is always within containerd's 76 characters and its
/// alphabet, whatever it is given; the labels carry them readably. A persistent workload's object,
/// which executes no run, kept by a driver alone in the namespace, keeps the digest it had before
/// runs and owners, so an upgraded driver finds it again.
pub(crate) fn container_id(owner: &str, workload: &str, intent: &str, run: &str) -> String {
    let mut named = if run.is_empty() {
        format!("{workload}\n{intent}")
    } else {
        format!("{workload}\n{intent}\n{run}")
    };
    // No workload identifier begins so, so no owned name is ever an unowned one.
    if !owner.is_empty() {
        named = format!("owner {owner}\n{named}");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_owners_name_a_workload_apart_and_a_driver_alone_names_it_as_before() {
        let alone = container_id("", "wl_a", "int_1", "");
        assert_eq!(alone, "korpis-7a0280376b19ddeaa94adc32db49e59afae7e3fc", "as before owners");
        let (a, b) =
            (container_id("a", "wl_a", "int_1", ""), container_id("b", "wl_a", "int_1", ""));
        assert!(a != b && a != alone && b != alone, "{a} {b} {alone}");
        assert!(is_container_id(&a) && is_container_id(&b));
    }
}
