// SPDX-License-Identifier: AGPL-3.0-or-later

//! Korpis's first-party key management provider (§5.5 of docs/06-storage.md, §2 of
//! docs/22-first-party.md): a tenant's data keys made and opened by the tenant's own OpenBao or
//! Vault Transit engine, each bound to its `<repository>/<workload>` context, and sealed by the SDK
//! to the one recipient a call names, so the provider never answers with a key in the clear.

pub mod config;
pub mod transit;

pub use transit::Transit;
