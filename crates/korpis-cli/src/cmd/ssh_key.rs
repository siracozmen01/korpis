// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis ssh-key …` (§9.9 of docs/08-identity.md, §8 of docs/13-surface-cli.md): the SSH keys
//! the signed-in person reaches their files with over SFTP. Adding or removing one needs a
//! sign-in within ten minutes; refused as not recent, the binary signs the terminal in again by
//! the device flow and asks once more (Finding 135 of docs/23-walkthroughs.md).

use super::{Answer, json};
use crate::connect::Connection;
use crate::error::CliError;
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use std::path::PathBuf;

/// `korpis ssh-key …`.
#[derive(Debug, Clone, Subcommand)]
pub enum SshKeyAction {
    /// Your SSH keys, by label, kind, and fingerprint.
    List,
    /// Registers a public key, as `ssh-keygen` writes it to a `.pub` file.
    Add {
        /// The public key's file.
        file: PathBuf,
        /// What to call it, at most 64 characters; the key's comment when not given.
        #[arg(long)]
        label: Option<String>,
    },
    /// Removes one of your SSH keys, by its identifier (`ssk_…`).
    Remove {
        /// The key.
        id: String,
    },
}

impl SshKeyAction {
    /// Whether it changes the keys, and so needs a recent sign-in.
    #[must_use]
    pub const fn changes(&self) -> bool {
        matches!(self, Self::Add { .. } | Self::Remove { .. })
    }
}

/// Whether `e` is the refusal of a change asked without a recent sign-in.
#[must_use]
pub fn not_recent(e: &CliError) -> bool {
    matches!(e, CliError::Api { reason, .. } if reason == "session.not_recent")
}

fn line(k: &pb::ProfileSshKey) -> String {
    let used = k.last_used_at.as_option().map_or("never used".to_owned(), |t| {
        let at = jiff::Timestamp::from_second(t.seconds).map(|t| t.to_string());
        format!("last used {}", at.unwrap_or_default())
    });
    let within = k.within.as_deref().map(|o| format!(", confined beneath {o}")).unwrap_or_default();
    format!("{}  {}  {} {}  {used}{within}", k.id, k.label, k.kind, k.fingerprint)
}

/// The key `file` holds and the label to give it: `label`, or the key's comment.
fn read(file: &PathBuf, label: Option<&str>) -> Result<(String, String), CliError> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| CliError::Local(format!("{}: {e}", file.display())))?;
    let key = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_owned();
    let comment = key.split_ascii_whitespace().nth(2).unwrap_or_default();
    Ok((key.clone(), label.unwrap_or(comment).to_owned()))
}

/// Runs `a`.
///
/// # Errors
/// The file could not be read, or the control plane refused.
pub async fn ssh_key(c: &Connection, a: &SshKeyAction) -> Result<Answer, CliError> {
    let profiles = pb::ProfileServiceClient::new(c.http(), c.config());
    match a {
        SshKeyAction::List => {
            let listed = profiles.list_ssh_keys(pb::ListSshKeysRequest::default()).await?;
            let listed = listed.into_owned();
            let human = if listed.keys.is_empty() {
                "no SSH keys".to_owned()
            } else {
                listed.keys.iter().map(line).collect::<Vec<_>>().join("\n")
            };
            Ok(Answer { json: json(&listed), human })
        }
        SshKeyAction::Add { file, label } => {
            let (key, label) = read(file, label.as_deref())?;
            let asked = pb::AddSshKeyRequest { key, label, ..Default::default() };
            let added = profiles.add_ssh_key(asked).await?.into_owned();
            let human = added.key.as_option().map_or_else(String::new, line);
            Ok(Answer { json: json(&added), human })
        }
        SshKeyAction::Remove { id } => {
            let asked = pb::RemoveSshKeyRequest { id: id.clone(), ..Default::default() };
            let removed = profiles.remove_ssh_key(asked).await?.into_owned();
            Ok(Answer { json: json(&removed), human: "removed".into() })
        }
    }
}
