// SPDX-License-Identifier: AGPL-3.0-or-later

//! One job, as the node runs it (§5.5 of docs/06-storage.md): the spec read from standard input,
//! the job confined, the work done against the store the spec grants, and the result written
//! where the helper reads it, whether the work succeeded or not. A restore is verified before it
//! is reported.

use crate::confine::{Bounds, Work, confine};
use crate::error::Error;
use crate::kept::{Chain, FromRepo, Kept};
use crate::keys::{KEY_LEN, Keys};
use crate::s3::{S3Config, S3Store};
use crate::{Content, Options, Repo, Restore, backup, restore, verify};
use buffa::Message as _;
use korpis_proto::korpis::backup::v1 as pb;
use korpis_proto::korpis::backup::v1::__buffa::oneof::job_spec::Work as W;
use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The largest spec a job reads.
const SPEC_LIMIT: u64 = 16 << 20;

/// Why a job could not begin.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// The spec did not arrive, or is not one.
    #[error("the job's spec: {0}")]
    Spec(String),
    /// The job could not be confined as the kernel offers.
    #[error(transparent)]
    Confine(#[from] crate::confine::ConfineError),
}

/// Reads the spec from `input`.
pub fn read_spec(input: impl std::io::Read) -> Result<pb::JobSpec, JobError> {
    let mut bytes = Vec::new();
    input.take(SPEC_LIMIT).read_to_end(&mut bytes).map_err(|e| JobError::Spec(e.to_string()))?;
    pb::JobSpec::decode_from_slice(&bytes).map_err(|e| JobError::Spec(e.to_string()))
}

/// The bounds `spec` sets.
pub fn bounds(spec: &pb::JobSpec) -> Result<Bounds, JobError> {
    let work = match &spec.work {
        Some(W::Backup(_)) => Work::Backup,
        Some(W::Restore(_)) => Work::Restore,
        None => return Err(JobError::Spec("no work".into())),
    };
    let store = spec.store.as_option().ok_or_else(|| JobError::Spec("no store".into()))?;
    let url =
        url::Url::parse(&store.endpoint).map_err(|e| JobError::Spec(format!("the store: {e}")))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| JobError::Spec("the store has no port".into()))?;
    let result = PathBuf::from(&spec.result);
    let state =
        result.parent().map(Path::to_path_buf).ok_or_else(|| JobError::Spec("no result".into()))?;
    let optional = |s: &str| (!s.is_empty()).then(|| PathBuf::from(s));
    Ok(Bounds {
        work,
        directory: PathBuf::from(&spec.directory),
        state,
        content: optional(&spec.content),
        cgroup: optional(&spec.cgroup),
        port,
    })
}

/// Confines this process to `spec`'s bounds; it must still be one thread.
pub fn confine_to(spec: &pb::JobSpec) -> Result<pb::Confinement, JobError> {
    Ok(confine(&bounds(spec)?)?)
}

/// Recipe content kept by the node's content store, beneath its root: a recipe's files in
/// `blobs/sha256/` and what fetches downloaded in `fetched/sha256/`, each by its sha256.
struct ContentDir(PathBuf);

impl crate::ContentSource for ContentDir {
    fn open(&self, sha256: &str) -> std::io::Result<Option<std::fs::File>> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(None);
        }
        for kept in ["blobs/sha256", "fetched/sha256"] {
            match std::fs::File::open(self.0.join(kept).join(sha256)) {
                Ok(f) => return Ok(Some(f)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }
}

fn s3(s: &korpis_proto::korpis::v1::S3Access) -> S3Config {
    S3Config {
        endpoint: s.endpoint.clone(),
        region: s.region.clone(),
        bucket: s.bucket.clone(),
        key_prefix: s.key_prefix.clone(),
        access_key_id: s.access_key_id.clone(),
        secret_access_key: s.secret_access_key.clone(),
        session_token: s.session_token.clone(),
        path_style: s.path_style,
    }
}

fn repo(spec: &pb::JobSpec) -> Result<Repo<S3Store>, Error> {
    let s = spec.store.as_option().ok_or_else(|| Error::Refused("no store".into()))?;
    let config = s3(s);
    let key: [u8; KEY_LEN] = spec
        .data_key
        .as_slice()
        .try_into()
        .map_err(|_| Error::Refused("a data key is 32 bytes".into()))?;
    Ok(Repo::new(Arc::new(S3Store::new(config)?), Arc::new(Keys::derive(&key))))
}

/// The repository's copy of recipe content, where it was granted on the store's own endpoint:
/// the job reaches no other (§5.2 of docs/06-storage.md).
fn kept(spec: &pb::JobSpec) -> Result<Option<Kept<S3Store>>, Error> {
    let (Some(store), Some(content)) = (spec.store.as_option(), spec.content_store.as_option())
    else {
        return Ok(None);
    };
    if content.endpoint != store.endpoint {
        tracing::warn!("recipe content granted on another endpoint is not reached");
        return Ok(None);
    }
    Ok(Some(Kept::new(Arc::new(S3Store::new(s3(content))?))))
}

/// Keeps in the repository each content `matched` names that it lacks, read from the node's
/// store. A failure leaves the backup whole, restorable wherever the content is held, and the
/// next backup tries again.
async fn keep_matched(kept: &Kept<S3Store>, local: &ContentDir, matched: &BTreeSet<String>) {
    for sha256 in matched {
        let kept_one = match crate::ContentSource::open(local, sha256) {
            Ok(Some(file)) => kept.keep(sha256, &file).await,
            Ok(None) => continue,
            Err(e) => Err(Error::Refused(e.to_string())),
        };
        if let Err(e) = kept_one {
            tracing::warn!(%sha256, error = %e, "recipe content was not kept in the repository");
        }
    }
}

/// Where a restore reads recipe content: the node's store, then the repository's copy where one
/// was granted, read into the job's own directory.
fn restore_content(spec: &pb::JobSpec) -> Result<Arc<dyn crate::ContentSource>, Error> {
    let local: Arc<dyn crate::ContentSource> = Arc::new(ContentDir(PathBuf::from(&spec.content)));
    let Some(kept) = kept(spec)? else { return Ok(local) };
    let dir = PathBuf::from(&spec.result).parent().map(Path::to_path_buf);
    let dir = dir.ok_or_else(|| Error::Refused("no job directory".into()))?;
    let handle = tokio::runtime::Handle::current();
    Ok(Arc::new(Chain(vec![local, Arc::new(FromRepo::new(kept, handle, dir))])))
}

/// Does `spec`'s work; what it did.
pub async fn run(spec: &pb::JobSpec) -> Result<pb::JobResult, Error> {
    let r = repo(spec)?;
    let concurrency = usize::try_from(spec.concurrency).unwrap_or(1).clamp(1, 64);
    let directory = PathBuf::from(&spec.directory);
    let mut result = pb::JobResult { ok: true, ..Default::default() };
    match &spec.work {
        Some(W::Backup(b)) => {
            let content =
                Content::new(b.content.iter().filter_map(|c| {
                    Some((c.size, <[u8; 32]>::try_from(c.sha256.as_slice()).ok()?))
                }));
            let referenced = content.clone();
            let parent = (!b.parent.is_empty()).then(|| b.parent.clone());
            let options = Options {
                source: directory,
                consistent: b.consistent,
                parent,
                content,
                concurrency,
            };
            result.backup = backup(&r, &b.backup, options).await?.into();
            if let Some(kept) = kept(spec)? {
                let local = ContentDir(PathBuf::from(&spec.content));
                keep_matched(&kept, &local, &referenced.matched()).await;
            }
        }
        Some(W::Restore(w)) => {
            let content = restore_content(spec)?;
            let what = Restore {
                backup: w.backup.clone(),
                path: w.path.clone(),
                target: directory.clone(),
                content,
                concurrency,
            };
            let done = restore(&r, what).await?;
            verify(&r, &w.backup, &w.path, directory).await?;
            result.restored = pb::Restored {
                files: done.files,
                directories: done.directories,
                bytes: done.bytes,
                skipped: done.skipped,
                verified: true,
                ..Default::default()
            }
            .into();
        }
        None => return Err(Error::Refused("no work".into())),
    }
    Ok(result)
}

/// Writes `result` to `path`, whole or not at all.
pub fn write_result(path: &Path, result: &pb::JobResult) -> std::io::Result<()> {
    let staging = path.with_extension("partial");
    std::fs::write(&staging, result.encode_to_vec())?;
    std::fs::File::open(&staging)?.sync_all()?;
    std::fs::rename(&staging, path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use crate::ContentSource as _;

    #[test]
    fn recipe_content_is_found_among_recipe_files_and_what_fetches_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let (file, fetched) = ("a".repeat(64), "b".repeat(64));
        for (kept, hex) in [("blobs/sha256", &file), ("fetched/sha256", &fetched)] {
            std::fs::create_dir_all(dir.path().join(kept)).unwrap();
            std::fs::write(dir.path().join(kept).join(hex), hex.as_bytes()).unwrap();
        }
        let content = ContentDir(dir.path().to_owned());
        assert!(content.open(&file).unwrap().is_some(), "a recipe's file");
        assert!(content.open(&fetched).unwrap().is_some(), "what a fetch downloaded");
        assert!(content.open(&"c".repeat(64)).unwrap().is_none(), "nothing else");
        assert!(content.open("../../etc/passwd").unwrap().is_none(), "and only by sha256");
    }
}
