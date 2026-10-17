// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis files sync`: a local directory written into a workload's, each directory made and each
//! file uploaded unless the workload holds it at the same size and at least as new. That is the
//! whole of its delta: a file session carries no digest, so a file changed in place without
//! changing its size or time is not seen as changed, and what changed is sent whole, not by
//! block. A symbolic link is not followed, and nothing in the workload is removed.

use super::remote::Remote;
use super::transfer;
use crate::cmd::Answer;
use crate::error::CliError;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// What a sync did.
#[derive(Debug, Default)]
struct Synced {
    sent: u64,
    skipped: u64,
    bytes: u64,
    links: u64,
}

fn local(path: &Path, e: &std::io::Error) -> CliError {
    CliError::Local(format!("{}: {e}", path.display()))
}

/// Makes directory `path`, which may be there already.
async fn made(r: &mut Remote, path: &str) -> Result<(), CliError> {
    let m = pb::FileMkdir { path: path.into(), ..Default::default() };
    match r.ask(Request::Mkdir(Box::new(m))).await {
        Ok(_) => Ok(()),
        Err(CliError::Api { code: pb::ErrorCode::ERROR_CODE_CONFLICT, ref reason, .. })
            if reason == "file.exists" =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Whether the workload's `there` is the local file of `meta` already: the same size, and
/// written no earlier.
fn current(there: Option<&pb::FileEntry>, meta: &std::fs::Metadata) -> bool {
    let Some(e) = there.filter(|e| e.kind.as_known() == Some(pb::FileKind::FILE_KIND_FILE)) else {
        return false;
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(i64::MAX, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let theirs = e.modified.as_option().map_or(i64::MIN, |t| t.seconds);
    e.size == meta.len() && theirs >= modified
}

/// Writes the local directory `from` into the workload's `to`.
///
/// # Errors
/// A local entry could not be read, or the node refused.
pub async fn sync(r: &mut Remote, from: &Path, to: &str, resume: bool) -> Result<Answer, CliError> {
    let mut s = Synced::default();
    made(r, to).await?;
    // Breadth first, by name, so the order is the same every run.
    let mut dirs: Vec<(PathBuf, String)> = vec![(from.to_owned(), to.to_owned())];
    while let Some((dir, there)) = dirs.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| local(&dir, &e))?
            .collect::<Result<_, _>>()
            .map_err(|e| local(&dir, &e))?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                return Err(CliError::Local(format!(
                    "{}: not a UTF-8 name",
                    entry.path().display()
                )));
            };
            let target = if there == "/" { format!("/{name}") } else { format!("{there}/{name}") };
            let meta =
                std::fs::symlink_metadata(entry.path()).map_err(|e| local(&entry.path(), &e))?;
            if meta.is_dir() {
                made(r, &target).await?;
                dirs.push((entry.path(), target));
            } else if meta.is_file() {
                file(r, (&entry.path(), &meta), &target, resume, &mut s).await?;
            } else {
                s.links += 1;
            }
        }
    }
    let human = format!(
        "{to}: {} files sent ({} bytes), {} already there, {} links and others not followed",
        s.sent, s.bytes, s.skipped, s.links
    );
    let json =
        json!({ "sent": s.sent, "bytes": s.bytes, "skipped": s.skipped, "notFollowed": s.links });
    Ok(Answer { json, human })
}

/// Uploads one file unless the workload holds it already.
async fn file(
    r: &mut Remote,
    (path, meta): (&Path, &std::fs::Metadata),
    target: &str,
    resume: bool,
    s: &mut Synced,
) -> Result<(), CliError> {
    let there = r.stat(target).await?;
    if current(there.as_ref(), meta) {
        s.skipped += 1;
        return Ok(());
    }
    let start = transfer::upload_from(r, target, meta.len(), resume).await?;
    s.bytes += transfer::upload(r, path, target, start).await?;
    s.sent += 1;
    Ok(())
}
