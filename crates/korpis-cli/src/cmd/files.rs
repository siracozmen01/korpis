// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis files` (§8 of docs/13-surface-cli.md): a workload's files through a relay, never
//! through the control plane, over the file session of §3.2 of docs/06-storage.md. A remote path
//! is written `w:WORKLOAD/PATH`, the path absolute within the workload as its volumes are
//! mounted (`w:mc-survival/data/world`), or `w:WORKLOAD:PATH` when the workload is named as
//! `project/name`.

mod remote;
mod sync;
mod transfer;

use super::{Answer, json};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use remote::Remote;
use serde_json::json;
use std::path::{Path, PathBuf};

/// `korpis files`.
#[derive(Debug, Clone, Subcommand)]
pub enum FilesAction {
    /// Lists a directory.
    Ls {
        /// The directory, `w:WORKLOAD/PATH`.
        remote: String,
        /// Open the session at this relay rather than the first the control plane names.
        #[arg(long)]
        relay: Option<String>,
    },
    /// Downloads a file.
    Get {
        /// The file, `w:WORKLOAD/PATH`.
        remote: String,
        /// Where it is written; its name, here, by default.
        local: Option<PathBuf>,
        /// Continue a partial download of the same file from the local file's size.
        #[arg(long)]
        resume: bool,
        /// Open the session at this relay rather than the first the control plane names.
        #[arg(long)]
        relay: Option<String>,
    },
    /// Uploads a file.
    Put {
        /// The local file.
        local: PathBuf,
        /// Where it is written, `w:WORKLOAD/PATH`.
        remote: String,
        /// Continue a partial upload of the same file from the size the node reports.
        #[arg(long)]
        resume: bool,
        /// Open the session at this relay rather than the first the control plane names.
        #[arg(long)]
        relay: Option<String>,
    },
    /// Uploads a directory, skipping each file the workload holds at the same size and at least
    /// as new; it removes nothing.
    Sync {
        /// The local directory.
        local: PathBuf,
        /// Where it is written, `w:WORKLOAD/PATH`.
        remote: String,
        /// Continue each partial upload from the size the node reports rather than start over.
        #[arg(long)]
        resume: bool,
        /// Open the session at this relay rather than the first the control plane names.
        #[arg(long)]
        relay: Option<String>,
    },
    /// Makes a directory.
    Mkdir {
        /// The directory, `w:WORKLOAD/PATH`.
        remote: String,
        /// Open the session at this relay rather than the first the control plane names.
        #[arg(long)]
        relay: Option<String>,
    },
    /// Renames a file or directory within the workload's volumes; it replaces nothing.
    Mv {
        /// What is renamed, `w:WORKLOAD/PATH`.
        remote: String,
        /// Its new path in the same workload.
        to: String,
        /// Open the session at this relay rather than the first the control plane names.
        #[arg(long)]
        relay: Option<String>,
    },
    /// Removes a file, or with `-r` a directory and everything in it.
    Rm {
        /// What is removed, `w:WORKLOAD/PATH`.
        remote: String,
        /// A directory, and everything in it.
        #[arg(short, long)]
        recursive: bool,
        /// Open the session at this relay rather than the first the control plane names.
        #[arg(long)]
        relay: Option<String>,
    },
}

/// `text` from the workload, safe to print on a terminal: a name is the tenant's to choose, and a
/// control character in it (an escape sequence that moves the cursor, retitles the window, or
/// writes the clipboard) is shown as its escape instead of acted on (Finding 82 of
/// docs/23-walkthroughs.md).
#[must_use]
pub fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { c.escape_default().to_string() } else { c.to_string() })
        .collect()
}

/// The workload and the path a remote argument names.
///
/// # Errors
/// It is not written `w:WORKLOAD/PATH` or `w:WORKLOAD:PATH`.
pub fn target(remote: &str) -> Result<(String, String), CliError> {
    let bad = || CliError::Local(format!("{remote}: a remote path is w:WORKLOAD/PATH"));
    let rest = remote.strip_prefix("w:").ok_or_else(bad)?;
    let (workload, path) = match rest.split_once(':') {
        Some((w, p)) => (w, p.to_owned()),
        None => match rest.split_once('/') {
            Some((w, p)) => (w, format!("/{p}")),
            None => (rest, "/".to_owned()),
        },
    };
    if workload.is_empty() || !path.starts_with('/') {
        return Err(bad());
    }
    Ok((workload.to_owned(), path))
}

async fn session(
    c: &Connection,
    remote: &str,
    write: bool,
    relay: Option<&String>,
) -> Result<(Remote, String), CliError> {
    let (named, path) = target(remote)?;
    let workload = resolve::workload(c, &named).await?;
    let at = relay.map(String::as_str);
    let r = match Remote::open(c, &workload, write, at).await {
        // A caller who may write reads through a writing session, which reads too (§3.2 of
        // docs/06-storage.md), though no grant of theirs names the reading action.
        Err(CliError::Api { code: pb::ErrorCode::ERROR_CODE_DENIED, .. }) if !write => {
            Remote::open(c, &workload, true, at).await?
        }
        r => r?,
    };
    Ok((r, path))
}

fn done(what: String) -> Answer {
    let json = json!({ "done": what });
    Answer { json, human: what + "\n" }
}

/// Runs `a`.
///
/// # Errors
/// The session could not be opened, a local file could not be read or written, or the node
/// refused.
pub async fn files(c: &Connection, a: &FilesAction) -> Result<Answer, CliError> {
    match a {
        FilesAction::Ls { remote, relay } => {
            let (mut r, path) = session(c, remote, false, relay.as_ref()).await?;
            ls(&mut r, &path).await
        }
        FilesAction::Get { remote, local, resume, relay } => {
            let (mut r, path) = session(c, remote, false, relay.as_ref()).await?;
            get(&mut r, &path, local.as_deref(), *resume).await
        }
        FilesAction::Put { local, remote, resume, relay } => {
            let (mut r, path) = session(c, remote, true, relay.as_ref()).await?;
            let len = std::fs::metadata(local)
                .map_err(|e| CliError::Local(format!("{}: {e}", local.display())))?
                .len();
            let start = transfer::upload_from(&mut r, &path, len, *resume).await?;
            let file = tokio::fs::File::open(local)
                .await
                .map_err(|e| CliError::Local(format!("{}: {e}", local.display())))?;
            let sent = transfer::upload(&mut r, (file, local), &path, start).await?;
            Ok(done(format!("{path}: {} bytes sent, {} in all", sent, start + sent)))
        }
        FilesAction::Sync { local, remote, resume, relay } => {
            let (mut r, path) = session(c, remote, true, relay.as_ref()).await?;
            sync::sync(&mut r, local, &path, *resume).await
        }
        FilesAction::Mkdir { remote, relay } => {
            let (mut r, path) = session(c, remote, true, relay.as_ref()).await?;
            let m = pb::FileMkdir { path: path.clone(), ..Default::default() };
            r.ask(Request::Mkdir(Box::new(m))).await?;
            Ok(done(format!("{path}: made")))
        }
        FilesAction::Mv { remote, to, relay } => {
            let (mut r, from) = session(c, remote, true, relay.as_ref()).await?;
            let m = pb::FileRename { from: from.clone(), to: to.clone(), ..Default::default() };
            r.ask(Request::Rename(Box::new(m))).await?;
            Ok(done(format!("{from}: renamed {to}")))
        }
        FilesAction::Rm { remote, recursive, relay } => {
            let (mut r, path) = session(c, remote, true, relay.as_ref()).await?;
            let m =
                pb::FileRemove { path: path.clone(), recursive: *recursive, ..Default::default() };
            r.ask(Request::Remove(Box::new(m))).await?;
            Ok(done(format!("{path}: removed")))
        }
    }
}

/// Every entry of directory `path`, page by page.
async fn ls(r: &mut Remote, path: &str) -> Result<Answer, CliError> {
    let mut entries = Vec::new();
    let mut after = String::new();
    loop {
        let l = pb::FileList { path: path.into(), after, limit: 1000, ..Default::default() };
        let Response::Entries(page) = r.ask(Request::List(Box::new(l))).await? else {
            return Err(CliError::Transport("a listing answered with no entries".into()));
        };
        entries.extend(page.entries);
        if page.next.is_empty() {
            break;
        }
        after = page.next;
    }
    let human = entries
        .iter()
        .map(|e| {
            let kind = match e.kind.as_known() {
                Some(pb::FileKind::FILE_KIND_DIRECTORY) => "dir ",
                Some(pb::FileKind::FILE_KIND_SYMLINK) => "link",
                Some(pb::FileKind::FILE_KIND_FILE) => "file",
                _ => "    ",
            };
            format!("{kind} {:>12} {}", e.size, printable(&e.name))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let json = serde_json::Value::Array(entries.iter().map(json).collect());
    Ok(Answer { json, human })
}

/// The name a download of `path` is written under here when none is given: the last component of
/// the path the caller asked for, never a name the node answered with, so a node cannot choose
/// where on the caller's machine its bytes land; and only a plain name, never `..` or a path.
///
/// # Errors
/// The path ends in no plain name.
pub fn local_name(path: &str) -> Result<String, CliError> {
    let name = path.rsplit('/').next().unwrap_or_default();
    let mut parts = Path::new(name).components();
    match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(n)), None) if n == name => Ok(name.to_owned()),
        _ => Err(CliError::Local(format!("{path}: no file name to write it under; name one"))),
    }
}

/// Downloads `path` to `to`, or to its own name here.
async fn get(
    r: &mut Remote,
    path: &str,
    to: Option<&Path>,
    resume: bool,
) -> Result<Answer, CliError> {
    let entry = r.stat(path).await?.ok_or_else(|| CliError::Local(format!("{path}: not there")))?;
    if entry.kind.as_known() != Some(pb::FileKind::FILE_KIND_FILE) {
        return Err(CliError::Local(format!("{path}: not a file")));
    }
    let to = match to {
        Some(to) => to.to_path_buf(),
        None => PathBuf::from(local_name(path)?),
    };
    let read = transfer::download(r, path, entry.size, (&to, resume)).await?;
    Ok(done(format!("{path}: {read} bytes written to {}", to.display())))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::{local_name, printable, target};

    #[test]
    fn a_name_from_the_workload_prints_its_control_characters_as_escapes() {
        assert_eq!(printable("level.dat"), "level.dat");
        assert_eq!(printable("a\u{1b}]0;owned\u{7}b"), "a\\u{1b}]0;owned\\u{7}b");
        assert_eq!(printable("x\ny"), "x\\ny");
        assert!(!printable("\u{9b}2J").chars().any(char::is_control));
    }

    #[test]
    fn a_download_is_named_by_the_path_asked_for_and_only_plainly() {
        assert_eq!(local_name("/data/world/level.dat").unwrap(), "level.dat");
        for bad in ["/data/..", "/data/.", "/", "/data/"] {
            assert!(local_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_remote_path_names_its_workload_and_an_absolute_path_within_it() {
        let t = |s: &str| target(s).unwrap();
        assert_eq!(t("w:mc-survival/data/world"), ("mc-survival".into(), "/data/world".into()));
        assert_eq!(t("w:mc-survival"), ("mc-survival".into(), "/".into()));
        assert_eq!(t("w:games/mc:/data"), ("games/mc".into(), "/data".into()));
        assert_eq!(t("w:wl_1/"), ("wl_1".into(), "/".into()));
        for bad in ["mc-survival/data", "w:", "w:/data", "w:mc:data"] {
            assert!(target(bad).is_err(), "{bad}");
        }
    }
}
