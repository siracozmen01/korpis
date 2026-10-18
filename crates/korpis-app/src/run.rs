// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runs (§4 of docs/05-scheduling.md): made and ended by the control plane as time, policies,
//! and power say, read by those who may read their workload, and cancelled by those who may stop
//! it.

mod control;
mod tend;
