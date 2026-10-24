// SPDX-License-Identifier: AGPL-3.0-or-later

//! The install itself, once the worker is confined: the actions decoded, run from where the spec
//! says, and each one's completion recorded in the output directory before the next begins, so
//! a worker that is killed part way is resumed at the action it did not finish.

use korpis_confine::root::CONTENT;
use korpis_core::recipe::installing::{InstallAction, detail};
use korpis_install::{Content, Volumes, run};
use korpis_proto::korpis::install::v1 as pb;
use rustix::fs::{Mode, OFlags};
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::path::Path;

/// Writes `bytes` as `name` in `out`, whole: beside it first, synced, then renamed over it.
///
/// # Errors
/// It could not be written.
pub fn record(out: &OwnedFd, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = format!(".{name}.tmp");
    let flags =
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(out, tmp.as_str(), flags, Mode::from_raw_mode(0o600))?;
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes)?;
    file.sync_all()?;
    rustix::fs::renameat(out, tmp.as_str(), out, name)?;
    Ok(())
}

/// The actions the spec carries.
fn actions(spec: &pb::InstallSpec) -> Result<Vec<InstallAction>, String> {
    spec.actions
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let value =
                korpis_wire::value::from_pb("actions", v).map_err(|e| format!("{i}: {e}"))?;
            detail::decode(&value).map_err(|e| format!("action {i}: {e}"))
        })
        .collect()
}

/// Copies the path of the backup's image `r` names into its new directory, as job `spec.id`;
/// 1 once it is copied, and why not otherwise.
fn restore(spec: &pb::InstallSpec, r: &pb::RestoreTree) -> (u32, Result<(), String>) {
    let mounts: Vec<(String, &Path)> =
        spec.volumes.iter().map(|v| (v.path.clone(), Path::new(v.path.as_str()))).collect();
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let copied = Volumes::open(&mounts).map_err(|e| format!("the volumes: {e}")).and_then(|v| {
        let (_, volume, rest) = v.locate(&r.volume)?;
        if !rest.is_empty() {
            return Err(format!("{} is no volume's root", r.volume));
        }
        let image = rustix::fs::open(CONTENT, flags, Mode::empty())
            .map_err(|e| format!("the backup's image: {e}"))?;
        korpis_install::tree::copy_tree((&image, &r.from), (volume, &r.into), &spec.id)
    });
    match copied {
        Ok(c) => {
            tracing::info!(entries = c.entries, bytes = c.bytes, "restored");
            (1, Ok(()))
        }
        Err(e) => (0, Err(e)),
    }
}

/// Runs the install `spec` describes, or its restore; how many actions have run, and why the rest
/// did not.
pub fn install(spec: &pb::InstallSpec, out: &OwnedFd) -> (u32, Result<(), String>) {
    if let Some(r) = spec.restore.as_option() {
        return restore(spec, r);
    }
    let from = spec.from;
    let failed = |e: String| (from, Err(e));
    let actions = match actions(spec) {
        Ok(a) => a,
        Err(e) => return failed(e),
    };
    let mounts: Vec<(String, &Path)> =
        spec.volumes.iter().map(|v| (v.path.clone(), Path::new(v.path.as_str()))).collect();
    let volumes = match Volumes::open(&mounts) {
        Ok(v) => v,
        Err(e) => return failed(format!("the volumes: {e}")),
    };
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let open =
        |dir: &str| rustix::fs::open(format!("{CONTENT}/{dir}").as_str(), flags, Mode::empty());
    let (blobs, fetched) = match (open("blobs"), open("fetched")) {
        (Ok(b), Ok(f)) => (b, f),
        (Err(e), _) | (_, Err(e)) => return failed(format!("the content: {e}")),
    };
    let content = Content { blobs, fetched, files: spec.files.clone() };
    let mut done = |i: usize| {
        let next = i.saturating_add(1);
        record(out, "progress", next.to_string().as_bytes())
    };
    let start = usize::try_from(from).unwrap_or(usize::MAX);
    match run(&actions, start, &volumes, &content, &mut done) {
        Ok(()) => (u32::try_from(actions.len()).unwrap_or(u32::MAX), Ok(())),
        Err(f) => (u32::try_from(f.index).unwrap_or(u32::MAX), Err(f.reason)),
    }
}
