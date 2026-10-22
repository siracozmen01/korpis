// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis ssh-key …` (§9.9 of docs/08-identity.md, §8 of docs/13-surface-cli.md): the SSH keys
//! the signed-in person reaches their files with over SFTP. Adding or removing one needs a
//! sign-in within ten minutes; refused as not recent, the binary signs the terminal in again by
//! the device flow and asks once more (Finding 135 of docs/23-walkthroughs.md). A key is added
//! with its signature over the account's name, which `ssh-keygen -Y sign` makes with the private
//! key beside the `.pub` file, or with the agent holding it, or which is given made elsewhere.

use super::{Answer, json};
use crate::connect::Connection;
use crate::error::CliError;
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};

/// The namespace a key's signature is made in, as `AddSshKeyRequest.signature` names it.
const NAMESPACE: &str = "korpis-ssh-key";

/// What a key's holder signs to add it to `account`, `usr_…`.
fn possession_message(account: &str) -> String {
    format!("korpis ssh-key add {account}")
}

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
        /// A signature made elsewhere, as `ssh-keygen -Y sign -n korpis-ssh-key` writes it over
        /// the text `korpis ssh-key add <your usr_…>`; made here with the key when not given.
        #[arg(long)]
        signature: Option<PathBuf>,
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

/// `message` signed by `ssh-keygen` with the private key beside `public`, or with the agent
/// holding it where there is none.
async fn signed(public: &Path, message: &str) -> Result<String, CliError> {
    use tokio::io::AsyncWriteExt as _;
    let private = public.with_extension("");
    let key = if public.extension().is_some_and(|e| e == "pub") && private.exists() {
        private
    } else {
        public.to_owned()
    };
    let mut child = tokio::process::Command::new("ssh-keygen")
        .args(["-q", "-Y", "sign", "-n", NAMESPACE, "-f"])
        .arg(&key)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| CliError::Local(format!("ssh-keygen, to sign with the key: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(message.as_bytes()).await.map_err(|e| CliError::Local(e.to_string()))?;
    }
    let out = child.wait_with_output().await.map_err(|e| CliError::Local(e.to_string()))?;
    if !out.status.success() {
        let why = format!("ssh-keygen could not sign with {}", key.display());
        return Err(CliError::Local(format!("{why}; give --signature made where the key is")));
    }
    String::from_utf8(out.stdout).map_err(|e| CliError::Local(e.to_string()))
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
        SshKeyAction::Add { file, label, signature } => {
            let (key, label) = read(file, label.as_deref())?;
            let signature = if let Some(made) = signature {
                std::fs::read_to_string(made)
                    .map_err(|e| CliError::Local(format!("{}: {e}", made.display())))?
            } else {
                let me = profiles.get_profile(pb::GetProfileRequest::default()).await?;
                signed(file, &possession_message(&me.into_owned().account)).await?
            };
            let asked = pb::AddSshKeyRequest { key, label, signature, ..Default::default() };
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use korpis_core::account::ssh_key::SshPublicKey;

    #[tokio::test]
    async fn the_key_beside_its_pub_file_signs_what_the_control_plane_checks() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("id_ed25519");
        let made = tokio::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&private)
            .status()
            .await;
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("skipped: ssh-keygen is not installed");
            return;
        }
        let public = private.with_extension("pub");
        let signature = signed(&public, &possession_message("usr_ada")).await.unwrap();
        let line = std::fs::read_to_string(&public).unwrap();
        let key = SshPublicKey::parse(&line).unwrap();
        assert_eq!(key.check_possession(&signature, "usr_ada"), Ok(()));
        assert_eq!(NAMESPACE, korpis_core::account::ssh_key::NAMESPACE, "one namespace");
        let core = korpis_core::account::ssh_key::possession_message("usr_ada");
        assert_eq!(possession_message("usr_ada"), core, "one text signed");
    }
}
