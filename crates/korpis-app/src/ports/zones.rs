// SPDX-License-Identifier: AGPL-3.0-or-later

//! Time zones by name (§4.1 of docs/05-scheduling.md). The control plane reads a schedule in the
//! zone its intent names, from its own zone database, which is the one thing that knows which
//! names exist and what their rules are; the domain reads no files, so the database is a port.

use jiff::tz::TimeZone;

/// What knows the time zones, by IANA name.
pub trait Zones: Send + Sync + std::fmt::Debug {
    /// The zone named `name`, if there is one.
    fn zone(&self, name: &str) -> Option<TimeZone>;
}

/// UTC alone, by its names: what an application handed no zone database knows.
#[derive(Debug, Clone, Copy, Default)]
pub struct UtcOnly;

impl Zones for UtcOnly {
    fn zone(&self, name: &str) -> Option<TimeZone> {
        matches!(name, "UTC" | "Etc/UTC").then_some(TimeZone::UTC)
    }
}
