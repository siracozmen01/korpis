// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file token for an SFTP client, asked for by the relay serving it (§3.1 of
//! docs/02-architecture.md, §9.9 of docs/08-identity.md). The relay has checked that the client
//! holds the SSH key it offered, which nothing lets it prove here, so the control plane believes
//! the relay, and records each first token under the relay's name. It finds the key's account,
//! evaluates that account's grants on the workload from the client's address, confined beneath
//! where the key was registered from a confined session, and mints a token for writing the
//! workload's files, or else for reading them.

use crate::App;
use crate::authz::MintToken;
use crate::authz::Minted;
use crate::caller::Caller;
use crate::error::AppError;
use crate::node::NodeProof;
use crate::ports::{Clock, EffectWrite as _, RecipeSource, SshKeyWrite as _, Store, Tx as _};
use korpis_core::account::ssh_key::SshPublicKey;
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::node::identity::{files_request_digest, relay_files_message};
use korpis_core::principal::Principal;
use std::net::IpAddr;
use uuid::Uuid;

/// What a relay asks a file token for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizeFiles {
    /// The SSH key the client holds, in its wire form.
    pub key: Vec<u8>,
    /// The workload the client named.
    pub workload: Uuid,
    /// Where the client connected from.
    pub client: IpAddr,
    /// The token this one renews, for a session already open.
    pub renews: Option<String>,
}

/// A file token, and what it allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesAuthorized {
    /// The token, with its route.
    pub minted: Minted,
    /// `workload.file.write`, or `workload.file.read` where only reading is allowed.
    pub action: &'static str,
    /// The account the key is registered to.
    pub account: Uuid,
}

const ACTIONS: [&str; 2] = ["workload.file.write", "workload.file.read"];

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The relay that signed `proof` over `cmd`, if it is a fresh signature of a relay not
    /// removed; for the control plane's own relay role, which asks in its own process, nothing.
    pub async fn sftp_relay(
        &self,
        relay: Uuid,
        proof: &NodeProof,
        cmd: &AuthorizeFiles,
    ) -> Result<Uuid, AppError> {
        let asked = files_request_digest(&cmd.key, cmd.workload, cmd.client, cmd.renews.as_deref());
        let message =
            |r: &str, s: i64, n: u32, nonce: &[u8]| relay_files_message((r, &asked), s, n, nonce);
        Ok(self.check_relay(relay, proof, &message).await?.id)
    }

    /// A file token for the account holding the key `cmd` names, asked for by `relay`, or by the
    /// control plane's own relay role where it is `None`.
    pub async fn authorize_files(
        &self,
        relay: Option<Uuid>,
        cmd: AuthorizeFiles,
    ) -> Result<FilesAuthorized, AppError> {
        let offered = SshPublicKey::from_blob(cmd.key).map_err(|_| AppError::Unauthenticated)?;
        let key = self.store.ssh_key_by_fingerprint(&offered.fingerprint()).await?;
        let key = key.filter(|k| k.key == offered).ok_or(AppError::Unauthenticated)?;
        let mut caller = Caller::new(Principal::User(key.account), Some(cmd.client), false);
        caller.within = key.within;
        let object = ObjectRef::new(Kind::Workload, cmd.workload);
        let (minted, action) = self.file_token(&caller, object, cmd.renews.as_deref()).await?;
        if cmd.renews.is_none() {
            self.sftp_authorized(&caller, (relay, &offered), object, action).await?;
        }
        Ok(FilesAuthorized { minted, action, account: key.account })
    }

    /// A token for writing `object`'s files, or else for reading them, for `caller`, renewing
    /// `renews` where it is given; the last refusal where neither is allowed.
    async fn file_token(
        &self,
        caller: &Caller,
        object: ObjectRef,
        renews: Option<&str>,
    ) -> Result<(Minted, &'static str), AppError> {
        let mut refused = AppError::Unauthenticated;
        for action in ACTIONS {
            let asked = MintToken {
                via: None,
                actions: vec![action.to_owned()],
                object,
                ttl: None,
                renews: renews.map(str::to_owned),
            };
            match self.mint_token(caller, asked).await {
                Ok(minted) => return Ok((minted, action)),
                Err(e) => refused = e,
            }
        }
        Err(refused)
    }

    /// Records a first file token minted for an SFTP client, on the workload, naming the relay
    /// that asked and the key, and marks the key used.
    async fn sftp_authorized(
        &self,
        caller: &Caller,
        (relay, key): (Option<Uuid>, &SshPublicKey),
        object: ObjectRef,
        action: &str,
    ) -> Result<(), AppError> {
        let relay = relay.map_or_else(|| "control-plane".to_owned(), |r| r.to_string());
        let source = caller.source_ip.map(|ip| ip.to_string()).unwrap_or_default();
        let effect = caller
            .effect("workload.file.sftp_authorized", Outcome::Succeeded)
            .about(object)
            .after(details([
                ("relay", relay.into()),
                ("fingerprint", key.fingerprint().into()),
                ("client", source.into()),
                ("action", action.into()),
            ]));
        let stored = self.store.ssh_key_by_fingerprint(&key.fingerprint()).await?;
        let mut tx = self.store.begin().await?;
        tx.append_effect(&effect).await?;
        if let Some(k) = stored {
            tx.ssh_key_used(k.id, self.clock.now()).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
