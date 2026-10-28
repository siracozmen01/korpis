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
    "workload.classify",
    "workload.migrate",
    "extension.read",
    "extension.install",
    "extension.uninstall",
    "event.read",
    "metering.read",
    "plan.read",
    "plan.approve",
    "plan.cancel",
    "volume.read",
    "volume.snapshot.create",
    "volume.snapshot.rollback",
    "volume.snapshot.delete",
    "volume.delete",
    "endpoint.read",
    "endpoint.write",
    "endpoint.release",
    "backup.create",
    "backup.browse",
    "backup.read",
    "backup.restore",
    "backup.prune",
    "repository.read",
    "repository.write",
    "domain.read",
    "domain.write",
    "domain.attest",
    "certificate.read",
    "certificate.write",
    "portal.read",
    "portal.write",
    "address_pool.read",
    "address_pool.write",
    "address_pool.attest",
    "edge_group.read",
    "edge_group.write",
    "stream.share",
    "grant.read",
    "grant.revoke",
    "template.write",
    "identity.binding.write",
    "identity.relay",
    "org.read",
    "org.create",
    "org.write",
    "org.suspend",
    "org.resume",
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
    "node.place",
    "node.remove",
];

/// Irreversible actions: excluded from set-subject and link-form grants by default, and routed
/// to explicit confirmation by every surface. One list, here (finding S35).
pub const IRREVERSIBLE: &[&str] = &[
    "workload.delete",
    "volume.delete",
    "volume.snapshot.rollback",
    "volume.snapshot.delete",
    "endpoint.release",
    "backup.prune",
    "node.remove",
];

/// Tenant-private actions: they do not cross an organization boundary implicitly (§3.3 of
/// docs/08-identity.md).
pub const PRIVATE: &[&str] = &[
    "workload.console.read",
    "workload.console.write",
    "workload.exec",
    "workload.file.read",
    "workload.file.write",
    "workload.secret.read",
    "workload.classify",
    "backup.browse",
    "backup.read",
    "backup.restore",
];

/// What a suspended organization's tenants may still do under their own grants (§2.1 of
/// docs/08-identity.md): read their grants, their tree, and the status and audit of what runs in
/// it, so a customer asked to pay can see what for. Reaching the service itself, a console, a
/// file, a backup's contents, is what suspension withholds, and every change is refused.
pub const WHILE_SUSPENDED: &[&str] = &[
    "grant.read",
    "org.read",
    "project.read",
    "workload.read",
    "plan.read",
    "node.read",
    "metering.read",
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

    /// Whether a grant scoped inside a suspended organization still authorizes it (§2.1 of
    /// docs/08-identity.md).
    #[must_use]
    pub fn stays_while_suspended(&self) -> bool {
        WHILE_SUSPENDED.contains(&self.0.as_str())
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
