// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Prepare` for the container tier (§4.2 of docs/04-runtimes.md): the shared preparing of
//! `korpis_driver_kit::prepare`, as the `oci` driver serves it, with the default process limit
//! its cgroup holds.

use crate::validate::DEFAULT_PIDS;
use korpis_core::workload::spec::IsolationTier;
use korpis_driver_kit::prepare::Serves;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;

/// The name the driver answers to in an intent's `runtime.driver`.
pub const NAME: &str = "oci";

/// What the `oci` driver serves.
pub const SERVES: Serves = Serves {
    name: NAME,
    tier: pb::IsolationTier::ISOLATION_TIER_CONTAINER,
    target: IsolationTier::Container,
    default_pids: Some(DEFAULT_PIDS),
};

/// Prepares `intent` for `workload` from the recipe document `manifest`.
pub fn prepare(
    workload: &str,
    intent: &pb::Intent,
    manifest: &[u8],
) -> Result<driver::PreparedSpec, driver::Rejection> {
    korpis_driver_kit::prepare::prepare(&SERVES, workload, (intent, manifest))
}
