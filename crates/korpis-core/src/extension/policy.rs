// SPDX-License-Identifier: AGPL-3.0-or-later

//! Who may install which extensions, as an operator says for the whole tree under a root
//! organization. A trust policy is an organization's own and a child may accept what its parent
//! does not, which is right for what a tenant runs; an operator restricting extensions, which act
//! as their installers inside the operator's installation, needs a rule no child loosens. So the
//! root organization's extension policy governs every installation beneath it: the registries an
//! extension's artifact may come from, the publishers one of whom must have signed it, and whether
//! anyone without `extension.install` over the root may install one at all. Every list empty and
//! tenants allowed is the default: open, as an installation that never set one is.

use crate::recipe::trust::Signer;

/// The most registries a policy lists.
pub const MAX_REGISTRIES: usize = 64;

/// The longest registry entry.
pub const MAX_REGISTRY: usize = 255;

/// An operator's rule over the extensions installed beneath a root organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionPolicy {
    /// Where an extension's artifact may come from: a registry host, or a repository prefix
    /// beneath one (`ghcr.io/acme`); empty for anywhere.
    pub registries: Vec<String>,
    /// Publishers, one of whom must have signed the artifact's digest; empty for anyone the
    /// installing organization's trust policy accepts.
    pub publishers: Vec<Signer>,
    /// Whether someone without `extension.install` over the root organization may install one.
    pub tenants_install: bool,
}

impl Default for ExtensionPolicy {
    fn default() -> Self {
        Self { registries: Vec::new(), publishers: Vec::new(), tenants_install: true }
    }
}

/// Whether `entry` is a registry host, or a repository prefix beneath one, as a policy lists it:
/// lowercase, no scheme, no tag or digest, no empty or dot path segment, no trailing slash.
#[must_use]
pub fn is_registry(entry: &str) -> bool {
    let fits = !entry.is_empty() && entry.len() <= MAX_REGISTRY;
    let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._:/".contains(c);
    fits && entry.chars().all(allowed)
        && !entry.contains("://")
        && entry.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

impl ExtensionPolicy {
    /// Whether an artifact of the repository `artifact` may be installed: anywhere, or at or
    /// beneath one of the listed registries, segment by segment, so `ghcr.io/acme` admits
    /// `ghcr.io/acme/dns` and not `ghcr.io/acmecorp/dns`.
    #[must_use]
    pub fn admits_registry(&self, artifact: &str) -> bool {
        self.registries.is_empty()
            || self.registries.iter().any(|r| {
                artifact == r
                    || artifact.strip_prefix(r.as_str()).is_some_and(|rest| rest.starts_with('/'))
            })
    }

    /// Whether every registry it lists is one, and at most [`MAX_REGISTRIES`]: the first that
    /// is not, by position.
    ///
    /// # Errors
    /// The position of the first entry that is not a registry, or the count when too many.
    pub fn check_registries(&self) -> Result<(), usize> {
        if self.registries.len() > MAX_REGISTRIES {
            return Err(self.registries.len());
        }
        match self.registries.iter().position(|r| !is_registry(r)) {
            Some(i) => Err(i),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_admits_what_lies_beneath_it_segment_by_segment() {
        let p = ExtensionPolicy {
            registries: vec!["ghcr.io/acme".into(), "registry.example:5000".into()],
            ..ExtensionPolicy::default()
        };
        assert!(p.admits_registry("ghcr.io/acme/dns"));
        assert!(p.admits_registry("registry.example:5000/x/y"));
        assert!(!p.admits_registry("ghcr.io/acmecorp/dns"), "a prefix, not a segment");
        assert!(!p.admits_registry("ghcr.io/other/dns"));
        assert!(ExtensionPolicy::default().admits_registry("anything/at/all"), "open by default");
    }

    #[test]
    fn a_registry_entry_is_a_host_or_a_path_beneath_one() {
        assert!(is_registry("ghcr.io/acme"));
        assert!(!is_registry("https://ghcr.io"));
        assert!(!is_registry("ghcr.io/acme/"), "no trailing slash");
        assert!(!is_registry("GHCR.io"));
        assert!(!is_registry("ghcr.io/../x"));
    }
}
