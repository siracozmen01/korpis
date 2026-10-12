// SPDX-License-Identifier: AGPL-3.0-or-later

//! The action vocabulary (§5 of docs/10-api.md). Actions are never renamed and never split.

use std::fmt;

/// An action name: one of the core vocabulary, or `ext.<extension>.<action>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Action(String);

/// Every core action. Adding one is additive; nothing here is ever removed or renamed.
pub const CORE: &[&str] = &[
    "workload.read",
    "workload.create",
    "workload.write",
    "workload.delete",
    "workload.start",
    "workload.stop",
    "workload.restart",
    "workload.console.read",
    "workload.console.write",
    "workload.exec",
    "workload.file.read",
    "workload.file.write",
    "workload.secret.read",
    "plan.read",
    "plan.approve",
    "plan.cancel",
    "volume.read",
    "volume.snapshot.create",
    "volume.delete",
    "endpoint.read",
    "endpoint.write",
    "endpoint.release",
    "backup.create",
    "backup.browse",
    "backup.read",
    "backup.restore",
    "backup.prune",
    "stream.share",
    "grant.read",
    "grant.revoke",
    "org.read",
    "org.create",
    "org.write",
    "org.suspend",
    "org.quota.write",
    "project.read",
    "project.create",
    "project.write",
    "project.delete",
    "node.read",
    "node.enroll",
    "node.cordon",
    "node.drain",
    "node.evacuate",
];

/// Irreversible actions: excluded from set-subject and link-form grants by default, and routed
/// to explicit confirmation by every surface. One list, here (finding S35).
pub const IRREVERSIBLE: &[&str] =
    &["workload.delete", "volume.delete", "endpoint.release", "backup.prune"];

/// Tenant-private actions: they do not cross an organization boundary implicitly (§3.3 of
/// docs/08-identity.md).
pub const PRIVATE: &[&str] = &[
    "workload.console.read",
    "workload.console.write",
    "workload.exec",
    "workload.file.read",
    "workload.file.write",
    "workload.secret.read",
    "backup.browse",
    "backup.read",
    "backup.restore",
];

/// Why a string is not an action.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown action {0:?}: not in the vocabulary and not ext.<extension>.<action>")]
pub struct UnknownAction(pub String);

impl Action {
    /// Parses an action, accepting the core vocabulary and namespaced extension actions.
    pub fn parse(name: &str) -> Result<Self, UnknownAction> {
        if CORE.contains(&name) || is_extension_action(name) {
            Ok(Self(name.to_owned()))
        } else {
            Err(UnknownAction(name.to_owned()))
        }
    }

    /// The action's name, e.g. `workload.restart`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether it cannot be undone, which keeps it out of links and set grants by default (§5.2 of
    /// docs/08-identity.md).
    #[must_use]
    pub fn is_irreversible(&self) -> bool {
        IRREVERSIBLE.contains(&self.0.as_str())
    }

    /// Whether it changes nothing: the `*.read` actions and `backup.browse`. Such an action is
    /// not proposed and approved, because approval governs change (§3.6 of
    /// docs/08-identity.md). Every other action, an extension's included, is a change.
    #[must_use]
    pub fn changes_nothing(&self) -> bool {
        let name = self.0.as_str();
        CORE.contains(&name) && (name.rsplit('.').next() == Some("read") || name == "backup.browse")
    }

    /// Whether it reaches a customer's data, which an ancestor never reaches implicitly (§7 of
    /// docs/08-identity.md).
    #[must_use]
    pub fn is_private(&self) -> bool {
        PRIVATE.contains(&self.0.as_str())
    }
}

fn is_extension_action(name: &str) -> bool {
    let mut parts = name.split('.');
    parts.next() == Some("ext")
        && parts.clone().count() >= 2
        && parts.all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        })
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_and_extension_namespace() {
        assert!(Action::parse("workload.restart").is_ok());
        assert!(Action::parse("ext.rcon.kick").is_ok());
        assert!(Action::parse("ext.rcon").is_err(), "an extension action needs a name");
        assert!(Action::parse("grant.issue").is_err(), "there is no grant.issue (§3.2)");
        assert!(Action::parse("workload.exec").unwrap().is_private());
        assert!(Action::parse("volume.delete").unwrap().is_irreversible());
    }
}
