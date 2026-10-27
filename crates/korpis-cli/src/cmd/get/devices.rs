// SPDX-License-Identifier: AGPL-3.0-or-later

//! The devices of a workload's volumes in block classes, as `korpis status` shows them (§2.1 of
//! docs/06-storage.md): one line each, saying whether its provider made it and attached it for
//! the lease the workload holds, and why its last answer failed while it is asked again.

use korpis_proto::korpis::v1 as pb;
use std::fmt::Write as _;

/// Appends a line for each device of `s`.
pub(super) fn block_devices(out: &mut String, s: &pb::WorkloadStatus) {
    for d in &s.block_devices {
        let state = match (d.made, d.attached) {
            (false, _) => "not made yet",
            (true, false) => "made, not attached",
            (true, true) => "attached",
        };
        let _ = writeln!(out, "device:    {} ({}): {state}", d.volume, d.storage_class);
        if !d.reason.is_empty() {
            let _ = writeln!(out, "           {}", d.reason);
        }
    }
}
