// SPDX-License-Identifier: AGPL-3.0-or-later

//! A provider registration (§3.3 of docs/16-extensions.md): one open stream an installation
//! serves one of its `provides` entries on, held by one control-plane replica.

use super::ProviderType;
use std::time::SystemTime;
use uuid::Uuid;

/// The most calls one stream may accept at once.
pub const MAX_CONCURRENCY: u32 = 1024;

/// The longest provider version accepted, in bytes.
pub const MAX_VERSION: usize = 64;

/// A provider stream, registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// Its identifier.
    pub id: Uuid,
    /// The installation serving.
    pub extension: Uuid,
    /// The organization it serves.
    pub organization: Uuid,
    /// What kind of provider.
    pub kind: ProviderType,
    /// Which of the installation's `provides` entries.
    pub name: String,
    /// The provider's own version.
    pub version: String,
    /// How many calls it accepts at once.
    pub concurrency: u32,
    /// An identity provider's: it signs people in (§9.7 of docs/08-identity.md).
    pub sign_in: bool,
    /// The control-plane process holding its stream.
    pub replica: Uuid,
    /// When it registered.
    pub connected_at: SystemTime,
    /// When its replica last confirmed the stream open.
    pub seen_at: SystemTime,
}
