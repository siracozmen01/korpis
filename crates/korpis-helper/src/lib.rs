// SPDX-License-Identifier: AGPL-3.0-or-later

//! The privileged helper (§6 of docs/02-architecture.md): the only component of the agent that
//! holds root. It is synchronous and never starts a second thread (§4 of docs/21-stack.md), it
//! serves the supervisor alone, one structured request at a time (`korpis.helper.v1`), and every
//! path it acts on is built from an identifier it checked, never taken from a request.

#![warn(missing_docs)]

pub mod error;
pub mod frame;
pub mod helper;
pub mod ids;
pub mod serve;
pub mod storage;
pub mod volume;
