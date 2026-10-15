// SPDX-License-Identifier: AGPL-3.0-or-later

//! The recipes whose content this node holds, for the scheduler's content locality (§2.2 of
//! docs/05-scheduling.md): a recipe is held once a `pull_recipe` step fetched its content here,
//! and the inventory is reported as `ContentStatus` whenever it grows and on every new session.
//! It is kept on disk, one digest per line, because a restarted agent still holds what it
//! fetched. Nothing on a node removes fetched content yet; whatever comes to remove it must
//! remove the recipe from here too, or the node is preferred for content it no longer has.

use crate::durable;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The inventory.
#[derive(Debug)]
pub struct Inventory {
    path: PathBuf,
    held: BTreeSet<String>,
    /// Whether the control plane holds what is held now.
    sent: bool,
}

impl Inventory {
    /// The inventory kept at `path`; empty when nothing was kept there.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let held = match std::fs::read_to_string(path) {
            Ok(text) => text.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path: path.to_owned(), held, sent: false })
    }

    /// Records that the content of the recipe with `digest` is held here.
    pub fn hold(&mut self, digest: &str) -> std::io::Result<()> {
        if digest.is_empty() || self.held.contains(digest) {
            return Ok(());
        }
        let mut held = self.held.clone();
        held.insert(digest.to_owned());
        let text = held.iter().fold(String::new(), |mut text, d| {
            text.push_str(d);
            text.push('\n');
            text
        });
        durable::replace(&self.path, text.as_bytes())?;
        self.held = held;
        self.sent = false;
        Ok(())
    }

    /// Makes the next report send everything again, as a new session needs.
    pub fn resend(&mut self) {
        self.sent = false;
    }

    /// What to send, when the control plane does not hold it yet; it is then taken as sent.
    pub fn report(&mut self) -> Option<agent::AgentMessage> {
        if self.sent {
            return None;
        }
        self.sent = true;
        let status = agent::ContentStatus {
            present: self.held.iter().cloned().collect(),
            ..Default::default()
        };
        Some(agent::AgentMessage {
            message: Some(Up::ContentStatus(Box::new(status))),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    fn present(m: Option<agent::AgentMessage>) -> Vec<String> {
        match m.and_then(|m| m.message) {
            Some(Up::ContentStatus(s)) => s.present,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn what_is_held_is_kept_reported_once_and_again_for_a_new_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inventory");
        let mut inventory = Inventory::open(&path).unwrap();
        assert!(present(inventory.report()).is_empty(), "an empty inventory is still reported");
        assert!(inventory.report().is_none());
        inventory.hold("sha256:bb").unwrap();
        inventory.hold("sha256:aa").unwrap();
        inventory.hold("sha256:aa").unwrap();
        assert_eq!(present(inventory.report()), ["sha256:aa", "sha256:bb"]);
        assert!(inventory.report().is_none(), "unchanged, not sent again");
        inventory.resend();
        assert_eq!(present(inventory.report()).len(), 2);
        let reopened = Inventory::open(&path).unwrap();
        assert_eq!(reopened.held.len(), 2, "kept across a restart");
    }
}
