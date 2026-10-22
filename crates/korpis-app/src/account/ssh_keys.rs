// SPDX-License-Identifier: AGPL-3.0-or-later

//! A signed-in person managing the SSH keys they reach their files with over SFTP (§9.9 of
//! docs/08-identity.md). Adding or removing one is a change to how they reach their account, so
//! it needs a recent sign-in, as every change to their factors does; and a key is added only with
//! its signature over the account's name, since a public key is public and naming one proves
//! nothing. A key added from a confined session records the organization it was confined beneath,
//! so a key a portal's holder could have seen reaches no further than the portal's organization;
//! a confined session sees and removes only the keys confined as it is.

use super::{label, own_account, record};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, SshKeyWrite as _, Store, StoreError, Tx as _, Unique};
use korpis_core::account::ssh_key::{SshKey, SshPublicKey};
use korpis_core::id::{Id, IdKind};
use uuid::Uuid;

fn registered(e: StoreError) -> AppError {
    match e {
        StoreError::Duplicate(Unique::SshKey) => {
            AppError::conflict("ssh_key.taken", "that key is registered already")
        }
        other => other.into(),
    }
}

/// Whether a session confined beneath `within`, or none, sees `key`: one that is not confined
/// sees every key of its account, and one that is sees those registered confined as it is.
fn sees(within: Option<Uuid>, key: &SshKey) -> bool {
    within.is_none() || key.within == within
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The caller's SSH keys, oldest first.
    pub async fn ssh_keys(&self, caller: &Caller) -> Result<Vec<SshKey>, AppError> {
        let account = own_account(caller)?;
        let mut keys = self.store.ssh_keys(account).await?;
        keys.retain(|k| sees(caller.within, k));
        Ok(keys)
    }

    /// Adds the key `line` names, as `ssh-keygen` writes it, under `name`, if `signature` is
    /// its signature over [`korpis_core::account::ssh_key::possession_message`] for the caller's
    /// account, proving that whoever adds it holds its private half.
    pub async fn add_ssh_key(
        &self,
        caller: &Caller,
        (line, signature): (&str, &str),
        name: &str,
    ) -> Result<SshKey, AppError> {
        let account = self.recently_signed_in(caller).await?;
        let key = SshPublicKey::parse(line).map_err(|e| AppError::invalid("key", e.to_string()))?;
        let named = Id::from_uuid(IdKind::User, account).to_string();
        key.check_possession(signature, &named)
            .map_err(|e| AppError::invalid("signature", e.to_string()))?;
        let key = SshKey {
            id: Uuid::now_v7(),
            account,
            key,
            label: label(name)?,
            within: caller.within,
            added_at: self.clock.now(),
            last_used_at: None,
        };
        let mut tx = self.store.begin().await?;
        tx.insert_ssh_key(&key).await.map_err(registered)?;
        let more = [
            ("key", key.id.to_string().into()),
            ("fingerprint", key.key.fingerprint().into()),
            ("kind", key.key.kind().name().into()),
        ];
        record(&mut tx, &caller.principal, "account.ssh_key_added", account, more).await?;
        tx.commit().await?;
        Ok(key)
    }

    /// Removes the caller's key `id`.
    pub async fn remove_ssh_key(&self, caller: &Caller, id: Uuid) -> Result<(), AppError> {
        let account = self.recently_signed_in(caller).await?;
        let keys = self.store.ssh_keys(account).await?;
        let Some(key) = keys.into_iter().find(|k| k.id == id && sees(caller.within, k)) else {
            return Err(AppError::NotFound { kind: "ssh key" });
        };
        let mut tx = self.store.begin().await?;
        if !tx.remove_ssh_key(account, id).await? {
            return Err(AppError::NotFound { kind: "ssh key" });
        }
        let more = [("key", id.to_string().into()), ("fingerprint", key.key.fingerprint().into())];
        record(&mut tx, &caller.principal, "account.ssh_key_removed", account, more).await?;
        tx.commit().await?;
        Ok(())
    }
}
