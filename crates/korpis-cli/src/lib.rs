// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `korpis` command, the reference client (§1 of docs/13-surface-cli.md): it calls only the
//! published API, every API operation has a command, and its JSON output is the API's own.

#![warn(missing_docs)]

pub mod apply;
pub mod call;
pub mod check;
pub mod cmd;
pub mod complete;
pub mod connect;
pub mod declared;
pub mod error;
pub mod files;
pub mod resolve;
pub mod sets;
pub mod show;
pub mod version;
