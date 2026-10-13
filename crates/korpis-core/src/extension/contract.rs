// SPDX-License-Identifier: AGPL-3.0-or-later

//! The extension contract this control plane speaks (§6 of docs/16-extensions.md): its versions
//! and its capabilities. A capability names an interface the control plane actually speaks, never
//! one it merely tolerates, so an extension requiring one that is absent is refused at install
//! rather than installed to wait for calls that never come.

use super::ExtensionManifest;

/// The contract versions this control plane serves, oldest first.
pub const CONTRACT_VERSIONS: &[&str] = &["korpis.extension.v1"];

/// The capabilities this control plane offers extensions, sorted. `provider.<type>` joins when
/// that type's calls are part of the contract (§5 of docs/16-extensions.md).
pub const CAPABILITIES: &[&str] = &[
    // Subscriptions and their delivery, §5 of docs/15-observability.md.
    "events.subscribe",
    // The workload credential socket of §9.5 of docs/08-identity.md.
    "identity.credential",
    // A provider's stream: registering, being pinged, and being cancelled, §3.3.
    "provider.serve",
];

/// Why a manifest cannot be served here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Incompatible {
    /// It speaks a contract version this control plane does not.
    #[error("it speaks {wanted}, and this control plane speaks {}", CONTRACT_VERSIONS.join(", "))]
    Version {
        /// The version it declares.
        wanted: String,
    },
    /// It requires capabilities this control plane does not offer.
    #[error(
        "it requires {}, which this control plane does not offer; it offers {}",
        missing.join(", "),
        CAPABILITIES.join(", ")
    )]
    Capabilities {
        /// What is missing, in the order the document names them.
        missing: Vec<String>,
    },
}

impl ExtensionManifest {
    /// Whether this control plane can serve it: its contract version, then everything it
    /// requires, all missing capabilities named at once.
    pub fn compatible(&self) -> Result<(), Incompatible> {
        if !CONTRACT_VERSIONS.contains(&self.api_version.as_str()) {
            return Err(Incompatible::Version { wanted: self.api_version.clone() });
        }
        let missing: Vec<String> =
            self.requires.iter().filter(|r| !CAPABILITIES.contains(&r.as_str())).cloned().collect();
        if missing.is_empty() { Ok(()) } else { Err(Incompatible::Capabilities { missing }) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(api_version: &str, requires: &[&str]) -> ExtensionManifest {
        ExtensionManifest {
            api_version: api_version.into(),
            requires: requires.iter().map(|r| (*r).to_owned()).collect(),
            needs: Vec::new(),
            egress: Vec::new(),
            subscribes: Vec::new(),
            provides: Vec::new(),
            actions: Vec::new(),
            ui_slots: Vec::new(),
        }
    }

    #[test]
    fn the_capabilities_are_sorted_and_distinct() {
        assert!(CAPABILITIES.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn an_extension_is_served_only_what_this_control_plane_speaks() {
        assert_eq!(manifest("korpis.extension.v1", &["provider.serve"]).compatible(), Ok(()));
        let later = manifest("korpis.extension.v2", &[]).compatible().unwrap_err();
        assert_eq!(later, Incompatible::Version { wanted: "korpis.extension.v2".into() });
        let missing = manifest("korpis.extension.v1", &["provider.dns", "events.subscribe", "x.y"])
            .compatible()
            .unwrap_err();
        let Incompatible::Capabilities { missing: names } = &missing else { panic!("{missing}") };
        assert_eq!(names, &["provider.dns", "x.y"], "every missing one, at once");
        assert!(missing.to_string().contains("it offers events.subscribe, identity"), "{missing}");
    }
}
