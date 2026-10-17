// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis files sync`: a local directory written into a workload's, each directory made and each
//! file uploaded unless the workload holds it at the same size and at least as new. That is the
//! whole of its delta: a file session carries no digest, so a file changed in place without
//! changing its size or time is not seen as changed, and what changed is sent whole, not by
//! block. A symbolic link is not followed, at any depth, and nothing in the workload is removed.

use super::remote::Remote;
use super::transfer;
use crate::cmd::Answer;
use crate::error::CliError;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use serde_json::json;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

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

/// What a local file is, as `fstat` on its open descriptor says.
struct Local {
    len: u64,
    modified: i64,
}

/// Whether the workload's `there` is the local file `l` already: the same size, and written no
/// earlier.
fn current(there: Option<&pb::FileEntry>, l: &Local) -> bool {
    let Some(e) = there.filter(|e| e.kind.as_known() == Some(pb::FileKind::FILE_KIND_FILE)) else {
        return false;
    };
    let theirs = e.modified.as_option().map_or(i64::MIN, |t| t.seconds);
    e.size == l.len && theirs >= l.modified
}

fn os(path: &Path, e: rustix::io::Errno) -> CliError {
    local(path, &std::io::Error::from(e))
}

/// The names in the open directory `dir`, `label`, in name order.
fn names(dir: &OwnedFd, label: &Path) -> Result<Vec<String>, CliError> {
    let mut names = Vec::new();
    for entry in Dir::read_from(dir).map_err(|e| os(label, e))? {
        let entry = entry.map_err(|e| os(label, e))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let name = std::str::from_utf8(name).map_err(|_| {
            CliError::Local(format!("{}: a name that is not UTF-8", label.display()))
        })?;
        names.push(name.to_owned());
    }
    names.sort();
    Ok(names)
}

/// `name` in the open directory `dir`, opened with `flags` and no link followed, if it is still
/// the entry `seen` described: a directory swapped for a link, or for another file, after it
/// was looked at is refused rather than read (Finding 81 of docs/23-walkthroughs.md).
fn reopen(
    dir: &OwnedFd,
    name: &str,
    flags: OFlags,
    (seen, label): (&rustix::fs::Stat, &Path),
) -> Result<OwnedFd, CliError> {
    let flags = flags | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(dir, name, flags, Mode::empty()).map_err(|e| os(label, e))?;
    let now = rustix::fs::fstat(&fd).map_err(|e| os(label, e))?;
    if (now.st_dev, now.st_ino) != (seen.st_dev, seen.st_ino) {
        return Err(CliError::Local(format!("{}: changed while it was read", label.display())));
    }
    Ok(fd)
}

/// Writes the local directory `from` into the workload's `to`. Every entry beneath `from` is
/// reached through its directory's open descriptor, never by its path again, so no link is
/// followed at any depth however the tree changes while it is read.
///
/// # Errors
/// A local entry could not be read, or the node refused.
pub async fn sync(r: &mut Remote, from: &Path, to: &str, resume: bool) -> Result<Answer, CliError> {
    let mut s = Synced::default();
    made(r, to).await?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let top = rustix::fs::open(from, flags, Mode::empty()).map_err(|e| os(from, e))?;
    let mut dirs: Vec<(OwnedFd, PathBuf, String)> = vec![(top, from.to_owned(), to.to_owned())];
    while let Some((dir, label, there)) = dirs.pop() {
        for name in names(&dir, &label)? {
            let target = if there == "/" { format!("/{name}") } else { format!("{there}/{name}") };
            let path = label.join(&name);
            let seen = rustix::fs::statat(&dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| os(&path, e))?;
            match FileType::from_raw_mode(seen.st_mode) {
                FileType::Directory => {
                    let inner = reopen(&dir, &name, flags, (&seen, &path))?;
                    made(r, &target).await?;
                    dirs.push((inner, path, target));
                }
                FileType::RegularFile => {
                    let fd =
                        reopen(&dir, &name, OFlags::RDONLY | OFlags::NONBLOCK, (&seen, &path))?;
                    let l = Local {
                        len: u64::try_from(seen.st_size).unwrap_or(0),
                        modified: seen.st_mtime,
                    };
                    file(r, (fd, &path, &l), &target, resume, &mut s).await?;
                }
                _ => s.links += 1,
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

/// Uploads one open file unless the workload holds it already.
async fn file(
    r: &mut Remote,
    (fd, path, l): (OwnedFd, &Path, &Local),
    target: &str,
    resume: bool,
    s: &mut Synced,
) -> Result<(), CliError> {
    let there = r.stat(target).await?;
    if current(there.as_ref(), l) {
        s.skipped += 1;
        return Ok(());
    }
    let start = transfer::upload_from(r, target, l.len, resume).await?;
    let file = tokio::fs::File::from_std(std::fs::File::from(fd));
    s.bytes += transfer::upload(r, (file, path), target, start).await?;
    s.sent += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::{names, reopen};
    use rustix::fs::{AtFlags, Mode, OFlags};

    #[test]
    fn an_entry_swapped_for_a_link_after_it_was_looked_at_is_refused_not_read() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("world.dat"), b"mine").unwrap();
        std::fs::write(t.path().join("secret"), b"not to be sent").unwrap();
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let dir = rustix::fs::open(t.path(), flags, Mode::empty()).unwrap();
        assert_eq!(names(&dir, t.path()).unwrap(), ["secret", "world.dat"]);
        let label = t.path().join("world.dat");
        let seen = rustix::fs::statat(&dir, "world.dat", AtFlags::SYMLINK_NOFOLLOW).unwrap();
        assert!(reopen(&dir, "world.dat", OFlags::RDONLY, (&seen, &label)).is_ok());

        // Swapped for a link to another file after it was looked at.
        std::fs::remove_file(&label).unwrap();
        std::os::unix::fs::symlink(t.path().join("secret"), &label).unwrap();
        assert!(reopen(&dir, "world.dat", OFlags::RDONLY, (&seen, &label)).is_err());
        // Swapped for another file of its own, renamed over it, so it has an inode of its own.
        std::fs::write(t.path().join("other"), b"other").unwrap();
        std::fs::rename(t.path().join("other"), &label).unwrap();
        assert!(reopen(&dir, "world.dat", OFlags::RDONLY, (&seen, &label)).is_err());
    }
}
