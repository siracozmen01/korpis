// SPDX-License-Identifier: AGPL-3.0-or-later

//! The machine's zone database (§4.1 of docs/05-scheduling.md): the zones its operating system
//! keeps under `/usr/share/zoneinfo` (or where `TZDIR` says), as updated with it, so a zone whose
//! rules change is right once the machine's database is.

use jiff::tz::TimeZone;
use korpis_app::ports::Zones;

/// The system's zones.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemZones;

impl Zones for SystemZones {
    fn zone(&self, name: &str) -> Option<TimeZone> {
        jiff::tz::db().get(name).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zone_the_machine_keeps_is_known_and_an_invented_one_is_not() {
        assert!(SystemZones.zone("UTC").is_some());
        assert!(SystemZones.zone("Nowhere/Atlantis").is_none());
    }
}
