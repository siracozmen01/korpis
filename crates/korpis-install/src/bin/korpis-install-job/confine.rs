// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the worker gives up once its root is built: every id but its user namespace's root, which
//! is its workload's; everything Landlock can take but writing beneath the volumes and its output
//! and reading the content; every connection; every capability but those an install needs to set
//! what it writes, which hold only over its workload's files; and any privilege something it
//! executed could gain.

use super::work;
use korpis_confine::root::CONTENT;
use korpis_confine::{CHOWN, ConfineError, DAC_OVERRIDE, FOWNER, FSETID};
use korpis_proto::korpis::install::v1 as pb;
use landlock::{
    ABI, Access as _, AccessFs, AccessNet, PathBeneath, Ruleset, RulesetAttr as _,
    RulesetCreatedAttr as _, RulesetStatus,
};
use rustix::fs::{Gid, Mode, OFlags, Uid};
use std::os::fd::OwnedFd;

fn err(e: impl std::fmt::Display) -> ConfineError {
    ConfineError(e.to_string())
}

fn dir(path: &str) -> Result<OwnedFd, ConfineError> {
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::open(path, flags, Mode::empty()).map_err(|e| err(format!("{path}: {e}")))
}

/// Becomes the user namespace's root: the workload's root, on the host its first id.
fn become_root() -> Result<(), ConfineError> {
    let (gid, uid) = (Gid::from_raw(0), Uid::from_raw(0));
    rustix::thread::set_thread_res_gid(gid, gid, gid).map_err(|e| err(format!("gid: {e}")))?;
    rustix::thread::set_thread_res_uid(uid, uid, uid).map_err(|e| err(format!("uid: {e}")))?;
    // The working directory was the host's, which is gone from this root.
    rustix::process::chdir("/").map_err(|e| err(format!("the root: {e}")))
}

/// Applies Landlock: all of each of `volumes` and of `out`, reading `content`, nothing else, and
/// no connection; whether the kernel enforces it.
fn landlock(volumes: &[&str], content: Option<&str>, out: OwnedFd) -> Result<bool, ConfineError> {
    let abi = ABI::V5;
    let (all, read) = (AccessFs::from_all(abi), AccessFs::from_read(abi));
    let mut created = Ruleset::default()
        .handle_access(all)
        .and_then(|r| r.handle_access(AccessNet::ConnectTcp | AccessNet::BindTcp))
        .and_then(Ruleset::create)
        .map_err(err)?
        .add_rule(PathBeneath::new(out, all))
        .map_err(err)?;
    for v in volumes {
        created = created.add_rule(PathBeneath::new(dir(v)?, all)).map_err(err)?;
    }
    if let Some(c) = content {
        created = created.add_rule(PathBeneath::new(dir(c)?, read)).map_err(err)?;
    }
    let status = created.restrict_self().map_err(err)?;
    Ok(status.ruleset != RulesetStatus::NotEnforced)
}

/// Confines the worker, then runs the install; what it did, or why it did not.
pub fn outcome(spec: &pb::InstallSpec, out: &OwnedFd) -> pb::InstallResult {
    let mut held =
        pb::Confinement { cgroup: !spec.cgroup.is_empty(), namespaces: true, ..Default::default() };
    if let Err(e) = confined(spec, out, &mut held) {
        return pb::InstallResult {
            error: e.to_string(),
            done: spec.from,
            confinement: held.into(),
            ..Default::default()
        };
    }
    let (done, ran) = work::install(spec, out);
    pb::InstallResult {
        ok: ran.is_ok(),
        error: ran.err().unwrap_or_default(),
        done,
        confinement: held.into(),
        ..Default::default()
    }
}

fn confined(
    spec: &pb::InstallSpec,
    out: &OwnedFd,
    held: &mut pb::Confinement,
) -> Result<(), ConfineError> {
    become_root()?;
    held.own_ids = true;
    let paths: Vec<&str> = spec.volumes.iter().map(|v| v.path.as_str()).collect();
    let content = (!spec.content.is_empty()).then_some(CONTENT);
    held.landlock = landlock(&paths, content, out.try_clone().map_err(err)?)?;
    held.capabilities = korpis_confine::keep_capabilities(&[CHOWN, FOWNER, DAC_OVERRIDE, FSETID])?;
    korpis_confine::no_new_privileges()?;
    held.no_new_privs = true;
    Ok(())
}
