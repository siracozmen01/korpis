// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backups (§5 of docs/06-storage.md): repositories, where an organization's backups are kept and
//! under whose key, and the backups of each workload's volumes.

pub mod job;
pub mod repository;
pub mod take;
