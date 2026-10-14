// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's data key, as each custody gives it (§5.5 of docs/06-storage.md). Under `operator`
//! custody the vault opens it, and a job's node is handed it. Under `tenant` custody the tenant's
//! key management service makes it and opens it, each time, sealed to a one-time recipient: a
//! job's node, which names one with its request, so the control plane never holds that key; or
//! the control plane itself, for its own read, whose recipient lives for that one opening. What
//! is stored is the key as the tenant's service wrapped it, which opens only through that service.

use super::job::JobGrant;
use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{BackupWrite as _, Clock, DataKey, RecipeSource, SealedKey, Store, Tx as _};
use korpis_core::backup::{Repository, TenantKey, key_context};
use korpis_core::extension::{CallOutcome, CallRequest, ProviderType};
use std::time::Duration;
use uuid::Uuid;

/// How long a key management service has to answer.
const KEY_CALL: Duration = Duration::from_secs(15);

/// A data key as a job is handed it.
#[derive(Debug)]
pub enum JobKey {
    /// Under `operator` custody, in the clear, over the node's authenticated stream.
    Clear(DataKey),
    /// Under `tenant` custody, sealed to the recipient the node named.
    Sealed(Vec<u8>),
}

fn unavailable(detail: String) -> AppError {
    AppError::Unavailable { what: "the tenant's key management service", detail }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// `workload`'s data key in `repo` for a job whose node made `recipient`; made at its first
    /// backup there when `make`, and otherwise required to exist.
    pub(super) async fn job_key(
        &self,
        (repo, workload): (&Repository, Uuid),
        make: bool,
        recipient: &[u8],
    ) -> Result<JobKey, AppError> {
        match &repo.kms {
            None => Ok(JobKey::Clear(self.data_key(repo, workload, make).await?)),
            Some(_) if recipient.is_empty() => Err(AppError::invalid(
                "recipient",
                "a repository under tenant custody hands its keys sealed to a recipient",
            )),
            Some(kms) => {
                Ok(JobKey::Sealed(self.tenant_key((repo, kms, workload), make, recipient).await?))
            }
        }
    }

    /// `workload`'s data key in `repo`, opened for the control plane's own read.
    pub(super) async fn own_key(
        &self,
        repo: &Repository,
        workload: Uuid,
    ) -> Result<DataKey, AppError> {
        let Some(kms) = &repo.kms else { return self.data_key(repo, workload, false).await };
        let failed = |e: crate::ports::VaultError| AppError::Internal(e.to_string());
        let recipient = self.vault()?.recipient().map_err(failed)?;
        let sealed = self.tenant_key((repo, kms, workload), false, &recipient.public()).await?;
        recipient.open(&sealed, key_context(repo.id, workload).as_bytes()).map_err(failed)
    }

    /// The tenant's service's answer: `workload`'s key sealed to `recipient`, made first when
    /// it has none and `make`. Two makers racing keep the first wrapped key and both open it.
    async fn tenant_key(
        &self,
        (repo, kms, workload): (&Repository, &TenantKey, Uuid),
        make: bool,
        recipient: &[u8],
    ) -> Result<Vec<u8>, AppError> {
        let target = CallTarget {
            organization: repo.organization,
            kind: ProviderType::KeyManagement,
            name: kms.provider.clone(),
        };
        let context = key_context(repo.id, workload).into_bytes();
        let ask = |request| self.key_call(&target, request);
        let mut tx = self.store.begin().await?;
        let stored = tx.workload_key(repo.id, workload).await?;
        tx.commit().await?;
        let wrapped = match stored {
            Some(w) => w.sealed,
            None if make => {
                let request = CallRequest::GenerateKey {
                    key: kms.key.clone(),
                    context: context.clone(),
                    recipient: recipient.to_vec(),
                };
                let (wrapped, sealed) = ask(request).await?;
                let kept =
                    SealedKey { sealed_by: format!("kms:{}", kms.provider), sealed: wrapped };
                let mut tx = self.store.begin().await?;
                let stored =
                    tx.keep_workload_key(repo.id, workload, &kept, self.clock.now()).await?;
                tx.commit().await?;
                if stored.sealed == kept.sealed {
                    return Ok(sealed);
                }
                stored.sealed
            }
            None => return Err(AppError::Internal("a backup's workload has no key".into())),
        };
        let request = CallRequest::UnwrapKey {
            key: kms.key.clone(),
            context,
            recipient: recipient.to_vec(),
            wrapped,
        };
        Ok(ask(request).await?.1)
    }

    /// One call to the tenant's service: the key wrapped (when made) and sealed.
    async fn key_call(
        &self,
        target: &CallTarget,
        request: CallRequest,
    ) -> Result<(Vec<u8>, Vec<u8>), AppError> {
        let called = self.call_provider(target, request, KEY_CALL).await?;
        match called.outcome {
            CallOutcome::KeyMaterial { wrapped, sealed } => Ok((wrapped, sealed)),
            CallOutcome::Failed(f) => Err(unavailable(f.reason)),
            _ => Err(unavailable("provider.malformed".into())),
        }
    }
}

impl JobGrant {
    /// Its key in the clear: the control plane's own reads open theirs, so theirs always is.
    ///
    /// # Errors
    /// The key is sealed to a node.
    pub fn clear_key(&self) -> Result<&DataKey, AppError> {
        match &self.data_key {
            JobKey::Clear(k) => Ok(k),
            JobKey::Sealed(_) => Err(AppError::Internal("a sealed key handed to a read".into())),
        }
    }
}
