// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `korpis` command, the reference client (§1 of docs/13-surface-cli.md): it calls only the
//! published API, every API operation has a command, and its JSON output is the API's own.

#![warn(missing_docs)]

pub mod call;
pub mod connect;
pub mod error;
pub mod version;
