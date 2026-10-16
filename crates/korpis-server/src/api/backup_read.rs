// SPDX-License-Identifier: AGPL-3.0-or-later

//! Browsing and reading a backup through the API (§5.4 of docs/06-storage.md): the use case
//! decides who may, records the read, and grants the workload's key with minutes of read access
//! to its prefix; this reads the repository with them, a directory's tree or a file's chunks,
//! and nothing else.

use super::wire::{self, Refuse, timestamp};
use super::{Api, error};
use connectrpc::{
    ConnectError, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use korpis_app::backup::job::JobGrant;
use korpis_app::backup::read::ReadFor;
use korpis_backup::s3::S3Store;
use korpis_backup::{Repo, browse};
use korpis_core::id::IdKind;
use korpis_proto::korpis::backup::v1 as format;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, UNIX_EPOCH};
use uuid::Uuid;

/// The repository a grant reaches.
fn repo_of(g: &JobGrant) -> Result<Repo<S3Store>, ConnectError> {
    crate::repos::repo_of(g).map_err(|e| unavailable(&e))
}

fn unavailable(e: &dyn std::fmt::Display) -> ConnectError {
    let detail = e.to_string();
    error::from_app(korpis_app::AppError::Unavailable { what: "the backup repository", detail })
}

/// The engine's error as the API says it: what the caller asked that is not in the backup, or
/// the repository failing it.
fn engine(e: &korpis_backup::Error) -> ConnectError {
    match e {
        korpis_backup::Error::Refused(why) => error::invalid("path", why),
        other => unavailable(other),
    }
}

/// That `path` is relative and stays in the backup.
fn checked(path: &str) -> Result<&str, ConnectError> {
    let mut bad = path.starts_with('/') || path.contains('\0');
    for part in path.split('/') {
        bad |= part == ".." || part == ".";
    }
    if bad {
        return Err(error::invalid("path", "a relative path in the backup"));
    }
    Ok(path)
}

fn entry_to_pb(e: &format::Entry) -> pb::BackupEntry {
    use format::EntryKind as F;
    let kind = match e.kind.as_known() {
        Some(F::ENTRY_KIND_FILE) => pb::BackupEntryKind::BACKUP_ENTRY_KIND_FILE,
        Some(F::ENTRY_KIND_DIRECTORY) => pb::BackupEntryKind::BACKUP_ENTRY_KIND_DIRECTORY,
        Some(F::ENTRY_KIND_SYMLINK) => pb::BackupEntryKind::BACKUP_ENTRY_KIND_SYMLINK,
        _ => pb::BackupEntryKind::BACKUP_ENTRY_KIND_SPECIAL,
    };
    let seconds = u64::try_from(e.mtime_seconds).unwrap_or(0);
    let modified = UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .and_then(|t| t.checked_add(Duration::from_nanos(u64::from(e.mtime_nanos))))
        .unwrap_or(UNIX_EPOCH);
    pb::BackupEntry {
        name: e.name.clone(),
        kind: kind.into(),
        size: e.size,
        mode: e.mode,
        modified: timestamp(modified).into(),
        link_target: e.link_target.clone(),
        ..Default::default()
    }
}

/// The pieces of a file as the API streams them: its length first, then its bytes, then the
/// engine's error if it ended in one.
fn pieces(
    size: u64,
    rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    reading: tokio::task::JoinHandle<korpis_backup::Result<u64>>,
) -> impl futures_util::Stream<Item = Result<pb::ReadBackupFileResponse, ConnectError>> + Send {
    let first = pb::ReadBackupFileResponse { size, ..Default::default() };
    let rest = futures_util::stream::unfold(Some((rx, reading)), |state| async move {
        let (mut rx, reading) = state?;
        if let Some(data) = rx.recv().await {
            let item = pb::ReadBackupFileResponse { data, ..Default::default() };
            return Some((Ok(item), Some((rx, reading))));
        }
        match reading.await {
            Ok(Ok(_)) => None,
            Ok(Err(e)) => Some((Err(engine(&e)), None)),
            Err(e) => Some((Err(error::internal(&e)), None)),
        }
    });
    futures_util::StreamExt::chain(futures_util::stream::once(async move { Ok(first) }), rest)
}

impl Api {
    /// Grants the read of `path` in `backup` for `what`, and opens the repository.
    async fn opened(
        &self,
        ctx: &RequestContext,
        (backup, path): (&str, &str),
        what: ReadFor,
    ) -> Result<(Repo<S3Store>, Uuid), ConnectError> {
        let caller = self.caller(ctx).await?;
        let id = wire::id(IdKind::Backup, "backup", backup).refuse()?;
        let grant = self.app.read_backup(&caller, (id, checked(path)?), what).await;
        Ok((repo_of(&grant.map_err(error::from_app)?)?, id))
    }

    pub(super) async fn browse(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::BrowseBackupRequest>,
    ) -> ServiceResult<pb::BrowseBackupResponse> {
        let (repo, id) = self.opened(&ctx, (request.backup, request.path), ReadFor::Browse).await?;
        let entries = browse::list(&repo, &id.to_string(), request.path).await;
        let entries = entries.map_err(|e| engine(&e))?.iter().map(entry_to_pb).collect();
        Response::ok(pb::BrowseBackupResponse { entries, ..Default::default() })
    }

    pub(super) async fn read_file(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ReadBackupFileRequest>,
    ) -> ServiceResult<ServiceStream<pb::ReadBackupFileResponse>> {
        let (repo, id) = self.opened(&ctx, (request.backup, request.path), ReadFor::Read).await?;
        let entry = browse::file(&repo, &id.to_string(), request.path).await;
        let entry = entry.map_err(|e| engine(&e))?;
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let size = entry.size;
        let reading = tokio::spawn(async move { browse::send(&repo, &entry, &tx).await });
        Response::stream_ok(pieces(size, rx, reading))
    }
}
