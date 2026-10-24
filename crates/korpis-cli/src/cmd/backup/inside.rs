// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block device's backup, read inside its image (§5.4 of docs/06-storage.md). The repository
//! holds such a backup as one file, the image, and only a guest's kernel parses what is in it, so
//! `korpis backup ls` and `cat` open a session of `backup.browse` or `backup.read` through a relay,
//! as `korpis files` opens one, and list or read beneath `/backup` there: the node restores the
//! image beside the workload's volume and its driver serves it from a guest. `--image` reads the
//! backup as the repository holds it instead.

use super::super::Answer;
use super::super::files::{self, Remote, transfer};
use crate::connect::Connection;
use crate::error::CliError;
use buffa::Message as _;
use korpis_proto::korpis::v1 as pb;

/// The action that lists a backup's files.
pub const BROWSE: &str = "backup.browse";
/// The action that reads them.
pub const READ: &str = "backup.read";
/// Where the backup's files are in the session.
const AT: &str = "/backup";

/// The workload whose backup `backup` is, when it is a block device's image, read inside.
///
/// # Errors
/// The backup could not be read.
pub async fn image_of(c: &Connection, backup: &str) -> Result<Option<String>, CliError> {
    let client = pb::BackupServiceClient::new(c.http(), c.config());
    let r = pb::GetBackupRequest { id: backup.to_owned(), ..Default::default() };
    let got = client.get_backup(r).await?.into_owned().backup.into_option().unwrap_or_default();
    Ok((!got.image_filesystem.is_empty()).then_some(got.workload))
}

/// A session of `action` on `backup` of `workload`.
async fn session(
    c: &Connection,
    (workload, backup): (&str, &str),
    action: &'static str,
) -> Result<Remote, CliError> {
    let params = pb::BackupFiles { backup: backup.to_owned(), ..Default::default() };
    Remote::opened(c, workload, (action, params.encode_to_vec()), None).await
}

/// `path` in the backup, as the session names it.
fn inside(path: &str) -> String {
    let path = path.trim_matches('/');
    if path.is_empty() { AT.to_owned() } else { format!("{AT}/{path}") }
}

/// Lists directory `path` of image backup `backup` of `workload`.
///
/// # Errors
/// The session could not be opened, or the node refused the listing.
pub async fn ls(
    c: &Connection,
    (workload, backup): (&str, &str),
    path: &str,
) -> Result<Answer, CliError> {
    let mut remote = session(c, (workload, backup), BROWSE).await?;
    files::ls(&mut remote, &inside(path)).await
}

/// Writes file `path` of image backup `backup` of `workload` to `out`; its length.
///
/// # Errors
/// The session could not be opened, the node refused, or `out` could not be written.
pub async fn cat(
    c: &Connection,
    (workload, backup): (&str, &str),
    path: &str,
    out: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<u64, CliError> {
    let mut remote = session(c, (workload, backup), READ).await?;
    let path = inside(path);
    let size = remote.stat(&path).await?.map(|e| e.size);
    let size = size.ok_or_else(|| CliError::Local(format!("{path}: not in the backup")))?;
    let read = transfer::download_into(&mut remote, (&path, size, 0), out).await;
    read.map_err(|e| e.unwrap_or_else(|e| CliError::Local(e.to_string())))
}
