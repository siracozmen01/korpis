// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a microVM's driver and its first process, `korpis-guest`, say to each other (§4.4 and §6
//! of docs/04-runtimes.md): the drives a guest is given and in which order, what the guest is to
//! run, written by the driver on the config drive before boot, and how the run ended, written by
//! the guest on the same drive before it powers off. Nothing else crosses: the guest is the
//! tenant's kernel, and the host reads back one fixed-size record, never a filesystem.

pub mod config;
pub mod drive;
pub mod status;

/// Why a config drive could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The drive does not hold a config of this format.
    #[error("not a korpis-guest config drive")]
    NotConfig,
    /// The config does not fit the drive.
    #[error("the config is {0} bytes, more than the drive holds")]
    TooLarge(usize),
    /// The config is not the format's JSON.
    #[error("the config is malformed: {0}")]
    Malformed(#[from] serde_json::Error),
    /// The drive could not be read or written.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
