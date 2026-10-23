// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Prepare` for the microvm tier (§4.2 of docs/04-runtimes.md): the shared preparing of
//! `korpis_driver_kit::prepare` as the `firecracker` driver serves it, with no default process
//! limit, since a guest's processes are its own kernel's to count, and refusing a console until
//! the guest's serial console reaches a stream.

use korpis_core::workload::spec::IsolationTier;
use korpis_driver_kit::prepare::Serves;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;

/// The name the driver answers to in an intent's `runtime.driver`.
pub const NAME: &str = "firecracker";

/// What the `firecracker` driver serves.
pub const SERVES: Serves = Serves {
    name: NAME,
    tier: pb::IsolationTier::ISOLATION_TIER_MICROVM,
    target: IsolationTier::MicroVm,
    default_pids: None,
};

/// Prepares `intent` for `workload` from the recipe document `manifest`.
pub fn prepare(
    workload: &str,
    intent: &pb::Intent,
    manifest: &[u8],
) -> Result<driver::PreparedSpec, driver::Rejection> {
    let spec = korpis_driver_kit::prepare::prepare(&SERVES, workload, (intent, manifest))?;
    if spec.stream.is_set() {
        return Err(driver::Rejection {
            reason: pb::UnsatisfiableReason::UNSATISFIABLE_REASON_PREPARE_REJECTED.into(),
            detail: "the firecracker driver offers no console yet; declare `console: none`".into(),
            ..Default::default()
        });
    }
    Ok(spec)
}
