// SPDX-License-Identifier: AGPL-3.0-or-later

//! Install jobs (§4 of docs/09-recipes.md): a `korpis-install-job` process running a recipe's
//! install actions beneath a workload's volumes, as the workload's own ids. The helper names each
//! volume's directory from its identifier, checks each path the workload sees it at, and refuses
//! an id range its ledger does not record for the workload; the worker mounts nothing else. It
//! writes its progress and its result in a directory of the job's made for it and given to the
//! workload's root, so nothing else of the job's directory (its pid, its log) is the worker's to
//! write.

use crate::error::{HelperError, HelperResult};
use crate::ids::Ledger;
use buffa::Message as _;
use korpis_core::workload::assignment::IdRange;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::install::v1 as pb;
use std::collections::BTreeSet;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

/// The file in a job's directory naming an install job, so its status is read as one.
pub(super) const KIND: &str = "install";

/// Where the worker writes, beneath the job's directory.
const OUT: &str = "out";

/// Checks a path a workload sees a volume at: absolute, each component plain, none beneath
/// `/.korpis`, where the worker keeps the content.
pub(crate) fn check_path(path: &str) -> HelperResult<()> {
    let refuse = |why: &str| Err(HelperError::Refused(format!("volume path {path:?} {why}")));
    let Some(rest) = path.strip_prefix('/') else { return refuse("is not absolute") };
    let parts: Vec<&str> = rest.split('/').collect();
    if rest.is_empty() || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return refuse("is not a plain path beneath the root");
    }
    if parts.first() == Some(&".korpis") {
        return refuse("is beneath /.korpis");
    }
    if path.len() > 4096 || path.contains('\0') {
        return refuse("is not a path");
    }
    Ok(())
}

/// Checks that `workload` holds the id range at `base`, as the ledger records: the worker runs as
/// those ids, so a request naming another's would give it that workload's files.
pub(crate) fn check_ids(ledger: &Ledger, workload: &str, base: u32) -> HelperResult<()> {
    crate::error::identifier("workload", workload)?;
    let range = IdRange { base, count: korpis_core::workload::assignment::IDS_PER_WORKLOAD };
    if range.slot().is_some() && ledger.holder(base) == Some(workload) {
        Ok(())
    } else {
        Err(HelperError::Refused(format!("workload {workload} holds no id range at {base}")))
    }
}

/// The spec for `work`, each volume's directory found by `directory`.
pub(crate) fn place(
    work: &helper::InstallWork,
    mut directory: impl FnMut(&str) -> HelperResult<PathBuf>,
) -> HelperResult<pb::InstallSpec> {
    let mut seen = BTreeSet::new();
    let mut volumes = Vec::new();
    for v in &work.volumes {
        check_path(&v.path)?;
        if !seen.insert(v.path.trim_end_matches('/')) {
            return Err(HelperError::Refused(format!("two volumes at {}", v.path)));
        }
        let dir = directory(&v.volume)?;
        volumes.push(pb::InstallVolume {
            path: v.path.clone(),
            volume: v.volume.clone(),
            directory: dir.to_string_lossy().into_owned(),
            ..Default::default()
        });
    }
    Ok(pb::InstallSpec {
        actions: work.actions.clone(),
        from: work.from,
        volumes,
        files: work.files.clone(),
        id_base: work.id_base,
        ..Default::default()
    })
}

/// Finishes `spec` for the job whose directory is `dir`: makes the directories the worker writes
/// in and mounts its root on, and marks the job an install job; the spec's bytes.
pub(super) fn finish(
    mut spec: pb::InstallSpec,
    id: &str,
    dir: &Path,
    (cgroup, content): (Option<&Path>, Option<&Path>),
) -> HelperResult<Vec<u8>> {
    let make = |name: &str| -> HelperResult<PathBuf> {
        let made = dir.join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&made)?;
        Ok(made)
    };
    let string = |p: &Path| p.to_string_lossy().into_owned();
    id.clone_into(&mut spec.id);
    let out = make(OUT)?;
    // The worker writes here as the workload's root, which is these host ids.
    std::os::unix::fs::chown(&out, Some(spec.id_base), Some(spec.id_base))?;
    spec.out = string(&out);
    spec.root = string(&make("root")?);
    spec.cgroup = cgroup.map(string).unwrap_or_default();
    spec.content = content.map(string).unwrap_or_default();
    std::fs::write(dir.join(KIND), "")?;
    Ok(spec.encode_to_vec())
}

/// Whether the job whose directory is `dir` is an install job.
pub(super) fn is_install(dir: &Path) -> bool {
    dir.join(KIND).exists()
}

/// What the install job in `dir` has recorded: its result once it wrote one, and its progress.
pub(super) fn recorded(dir: &Path) -> HelperResult<(Option<pb::InstallResult>, u32)> {
    let out = dir.join(OUT);
    let progress = match std::fs::read_to_string(out.join("progress")) {
        Ok(text) => text.trim().parse().unwrap_or(0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e.into()),
    };
    match std::fs::read(out.join("result")) {
        Ok(bytes) => {
            let result = pb::InstallResult::decode_from_slice(&bytes)
                .map_err(|e| HelperError::Failed(format!("an install's result: {e}")))?;
            let progress = progress.max(result.done);
            Ok((Some(result), progress))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((None, progress)),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::check_path;

    #[test]
    fn a_volume_path_is_absolute_plain_and_not_the_worker_s_own() {
        for ok in ["/data", "/data/world", "/srv/game.d/x"] {
            assert!(check_path(ok).is_ok(), "{ok}");
        }
        for bad in ["data", "/", "/data/../etc", "/data//x", "/./data", "/.korpis", "/.korpis/x"] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
    }
}
