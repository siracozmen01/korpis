// SPDX-License-Identifier: AGPL-3.0-or-later

//! Calling a backup target provider (§5.6 of docs/06-storage.md): access to one workload's
//! prefix for one job, and the listing and deletion the control plane prunes with. Every request
//! is checked before a provider sees it; access is read once and its secret then removed from the
//! record of the call, so the database holds it only for as long as the call took to be read.

use crate::App;
use crate::error::AppError;
use crate::extension::{CallTarget, Called};
use crate::ports::{Clock, ProviderCallWrite, RecipeSource, Store, Tx};
use korpis_core::extension::{
    AccessMode, CallFailure, CallOutcome, CallRequest, MAX_ACCESS, MAX_KEYS, MIN_ACCESS,
    ProviderType, StoredObject, is_beneath, is_prefix,
};
use std::net::SocketAddr;
use std::time::Duration;

/// How long a call's own deadline is, for backup target calls: they are control-path calls.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

fn backup_target(target: &CallTarget) -> Result<(), AppError> {
    if target.kind == ProviderType::BackupTarget {
        Ok(())
    } else {
        Err(AppError::invalid("provider", "a backup target provider"))
    }
}

fn prefix(p: &str) -> Result<(), AppError> {
    if is_prefix(p) {
        Ok(())
    } else {
        let why = "a relative key prefix ending in /, of letters, digits, '.', '_', and '-'";
        Err(AppError::invalid("prefix", why))
    }
}

/// `o`, named beneath `on`: a listing that strays outside it is the target's malfunction.
fn beneath(mut o: StoredObject, on: &str) -> Result<StoredObject, AppError> {
    let Some(name) = o.key.strip_prefix(on).map(str::to_owned) else {
        let detail = format!("a listing named {} outside {on}", o.key);
        return Err(AppError::Unavailable { what: "the backup target", detail });
    };
    o.key = name;
    Ok(o)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Access to `prefix` for `ttl`, for one job's node. Internal: the job decides who may.
    pub async fn grant_backup_access(
        &self,
        target: &CallTarget,
        on: &str,
        mode: AccessMode,
        ttl: Duration,
    ) -> Result<Called, AppError> {
        backup_target(target)?;
        prefix(on)?;
        if !(MIN_ACCESS..=MAX_ACCESS).contains(&ttl) {
            let why = format!("{} to {} seconds", MIN_ACCESS.as_secs(), MAX_ACCESS.as_secs());
            return Err(AppError::invalid("ttl", why));
        }
        let request = CallRequest::GrantAccess { prefix: on.to_owned(), mode, ttl };
        let called = self.call_provider(target, request, CALL_TIMEOUT).await?;
        let mut tx = self.store.begin().await?;
        tx.redact_call(called.id).await?;
        tx.commit().await?;
        self.located(target, called).await
    }

    /// `called` with the published address of the endpoint a store's provider serves it on put
    /// in its access, where the provider serves the store itself (§5.6 of docs/06-storage.md):
    /// every reader, a job on any node or the control plane, reaches it there. Without one it
    /// fails `backup.unreachable`.
    async fn located(&self, target: &CallTarget, mut called: Called) -> Result<Called, AppError> {
        let CallOutcome::Access(a) = &mut called.outcome else { return Ok(called) };
        if a.s3.served_by.is_empty() {
            return Ok(called);
        }
        if let Some(at) = self.published(target, &a.s3.served_by).await? {
            a.s3.endpoint = format!("https://{at}");
            a.s3.served_by.clear();
        } else {
            let why = "the endpoint the target serves on holds no port of its node";
            called.outcome = CallOutcome::Failed(CallFailure::of("backup.unreachable", why, true));
        }
        Ok(called)
    }

    /// Where the provider of `target` publishes its endpoint `name`: its node's advertised
    /// address and the port the node holds for it.
    async fn published(
        &self,
        target: &CallTarget,
        name: &str,
    ) -> Result<Option<SocketAddr>, AppError> {
        let serving = self.store.registrations(target.organization, target.kind, &target.name);
        let Some(r) = serving.await?.into_iter().next() else { return Ok(None) };
        let Some(x) = self.store.extension(r.extension).await? else { return Ok(None) };
        let endpoints = self.store.endpoints(x.workload).await?;
        let Some(e) = endpoints.into_iter().find(|e| e.name == name) else { return Ok(None) };
        let Some(port) = e.port else { return Ok(None) };
        let node = self.store.node(e.node).await?;
        let address = node.and_then(|n| n.advertised).and_then(|a| a.addresses.first().copied());
        Ok(address.map(|ip| SocketAddr::new(ip, port)))
    }

    /// The keys under `prefix` after `after`, at most `limit`, for pruning. Internal.
    pub async fn list_backup_objects(
        &self,
        target: &CallTarget,
        on: &str,
        after: &str,
        limit: u32,
    ) -> Result<Called, AppError> {
        backup_target(target)?;
        prefix(on)?;
        if limit == 0 || usize::try_from(limit).map_or(true, |n| n > MAX_KEYS) {
            return Err(AppError::invalid("limit", format!("1 to {MAX_KEYS}")));
        }
        let request =
            CallRequest::ListObjects { prefix: on.to_owned(), after: after.to_owned(), limit };
        self.call_provider(target, request, CALL_TIMEOUT).await
    }

    /// Every object beneath `prefix`, named beneath it, a page at a time. Internal.
    pub async fn list_all_objects(
        &self,
        target: &CallTarget,
        on: &str,
    ) -> Result<Vec<StoredObject>, AppError> {
        let limit = u32::try_from(MAX_KEYS).unwrap_or(u32::MAX);
        let mut all: Vec<StoredObject> = Vec::new();
        let mut after = String::new();
        loop {
            let page = self.list_backup_objects(target, on, &after, limit).await?;
            let (objects, more) = match page.outcome {
                CallOutcome::Objects { objects, more } => (objects, more),
                CallOutcome::Failed(f) => {
                    return Err(AppError::Unavailable {
                        what: "the backup target",
                        detail: f.reason,
                    });
                }
                _ => {
                    return Err(AppError::Unavailable {
                        what: "the backup target",
                        detail: "malformed".into(),
                    });
                }
            };
            let Some(last) = objects.last().map(|o| o.key.clone()) else { break };
            all.extend(objects.into_iter().map(|o| beneath(o, on)).collect::<Result<Vec<_>, _>>()?);
            if !more || last <= after {
                break;
            }
            after = last;
        }
        Ok(all)
    }

    /// Deletes `keys`, each beneath `prefix`, with the target's own credential: the second phase
    /// of a prune, which only the control plane runs. Internal.
    pub async fn delete_backup_objects(
        &self,
        target: &CallTarget,
        on: &str,
        keys: Vec<String>,
    ) -> Result<Called, AppError> {
        backup_target(target)?;
        prefix(on)?;
        if keys.is_empty() || keys.len() > MAX_KEYS {
            return Err(AppError::invalid("keys", format!("1 to {MAX_KEYS}")));
        }
        if let Some(i) = keys.iter().position(|k| !is_beneath(k, on)) {
            return Err(AppError::invalid(format!("keys[{i}]"), "beneath the prefix"));
        }
        self.call_provider(target, CallRequest::DeleteObjects { keys }, CALL_TIMEOUT).await
    }
}
