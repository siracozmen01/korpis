// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a guest is kept told of its workload's directory on the host (§6 of docs/04-runtimes.md):
//! whether the node serves the API relay, and the managed files in `config/` and the
//! dependencies' files in `deps/`, which a container reads through its mount and a guest is sent
//! a copy of as they change. The directory is looked at every two seconds, and more often until
//! the guest has taken what it holds, since its entry waits for the first sync. A file is read
//! through no link and only if it is a regular one; a directory holds at most
//! [`MOST_FILES`](control::MOST_FILES) and a file is at most a mebibyte, the guest's copies at
//! most eight in all, and what is past those is left out and said.

use crate::agent;
use korpis_guest::control::{self, Place};
use std::collections::BTreeMap;
use std::hash::{Hash as _, Hasher as _};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The API relay's name in the workload's directory.
const API: &str = "api.sock";
/// How often the directory is looked at once the guest has what it holds.
const LOOK: Duration = Duration::from_secs(2);
/// How often until then.
const SOON: Duration = Duration::from_millis(200);
/// How long a guest's agent is given to take what it is told.
const TELL: Duration = Duration::from_secs(5);
/// How long it is given to take a whole sync.
const SYNC: Duration = Duration::from_secs(30);
/// The most bytes of the workload's files a guest is sent: its `/run` is a small tmpfs.
const MOST_SENT: u64 = 8 << 20;

/// One of the workload's files on the host, as last read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Its directory.
    pub place: Place,
    /// Its name there.
    pub name: String,
    /// What it holds.
    pub bytes: Vec<u8>,
}

/// What a guest was last sent: each file by its place and name, with a digest of its bytes.
pub type Sent = BTreeMap<(Place, String), u64>;

/// Keeps the guest whose vsock device's socket is `vsock` told of its workload's directory
/// `identity`, for as long as the guest's relays are served.
pub async fn keeping(vsock: PathBuf, identity: PathBuf) {
    let mut serves = None;
    let mut sent: Option<Sent> = None;
    loop {
        let wanted = control::Relays { api: identity.join(API).exists() };
        if serves != Some(wanted) {
            serves = agent::relays(&vsock, wanted, TELL).await.ok();
        }
        let files = read(&identity);
        if sent.as_ref() != Some(&digests(&files)) {
            let synced = sync(&vsock, &files, sent.as_ref());
            sent = tokio::time::timeout(SYNC, synced).await.ok().and_then(Result::ok);
        }
        let done = serves.is_some() && sent.is_some();
        tokio::time::sleep(if done { LOOK } else { SOON }).await;
    }
}

/// The files of `identity`'s `config/` and `deps/`, in order, within the bounds.
#[must_use]
pub fn read(identity: &Path) -> Vec<File> {
    let mut files = Vec::new();
    let mut total = 0_u64;
    for place in Place::ALL {
        let dir = identity.join(place.dir());
        let is_dir = std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir());
        let Some(entries) = is_dir.then(|| std::fs::read_dir(&dir).ok()).flatten() else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| control::plain(n))
            .collect();
        names.sort();
        if names.len() > control::MOST_FILES {
            tracing::warn!(dir = %dir.display(), "more files than a guest is sent; the rest left out");
            names.truncate(control::MOST_FILES);
        }
        for name in names {
            let Ok(bytes) = regular(&dir.join(&name)) else { continue };
            total += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            if total > MOST_SENT {
                tracing::warn!(dir = %dir.display(), "more bytes than a guest is sent; the rest left out");
                return files;
            }
            files.push(File { place, name, bytes });
        }
    }
    files
}

/// What the regular file at `path` holds, opened through no link, if it is at most a file's
/// most.
fn regular(path: &Path) -> io::Result<Vec<u8>> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file = std::fs::File::from(rustix::fs::open(path, flags, Mode::empty())?);
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > control::MOST_FILE {
        return Err(io::Error::other("not a regular file within the most"));
    }
    let mut bytes = Vec::new();
    file.take(control::MOST_FILE + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > control::MOST_FILE {
        return Err(io::Error::other("a file grew past the most"));
    }
    Ok(bytes)
}

/// Each of `files`' digests by its place and name.
#[must_use]
pub fn digests(files: &[File]) -> Sent {
    files
        .iter()
        .map(|f| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            f.bytes.hash(&mut h);
            ((f.place, f.name.clone()), h.finish())
        })
        .collect()
}

/// Sends the guest through `vsock` those of `files` whose bytes `before` does not hold, then what
/// each directory keeps; what it then holds.
///
/// # Errors
/// The guest's agent cannot be reached, or refused a part.
pub async fn sync(vsock: &Path, files: &[File], before: Option<&Sent>) -> io::Result<Sent> {
    let now = digests(files);
    let mut frames = Vec::new();
    for f in files {
        let key = (f.place, f.name.clone());
        if before.and_then(|b| b.get(&key)) == now.get(&key) {
            continue;
        }
        let mut chunks: Vec<&[u8]> = f.bytes.chunks(control::PART).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        let last = chunks.len() - 1;
        for (i, data) in chunks.into_iter().enumerate() {
            let offset = u64::try_from(i * control::PART).unwrap_or(u64::MAX);
            let part =
                control::Part { place: f.place, name: &f.name, offset, last: i == last, data };
            frames.push((control::Kind::Put, part.encode()));
        }
    }
    for place in Place::ALL {
        let names: Vec<&str> =
            files.iter().filter(|f| f.place == place).map(|f| f.name.as_str()).collect();
        frames.push((control::Kind::Keep, control::keep(place, &names)));
    }
    agent::store(vsock, &frames).await?;
    Ok(now)
}
