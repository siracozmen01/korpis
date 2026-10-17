// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the worker gives up once its root is built: every id but its user namespace's root, which
//! is its workload's; everything Landlock can take but the volumes, all of them in a session that
//! writes and their reading in one that reads, and listing the root they are mounted in; every
//! connection; every capability but those a file's owner needs, which hold only over its
//! workload's files; and any privilege something it executed could gain, though it executes
//! nothing.

use korpis_confine::{CHOWN, ConfineError, DAC_OVERRIDE, DAC_READ_SEARCH, FOWNER};
use landlock::{
    ABI, Access as _, AccessFs, AccessNet, PathBeneath, Ruleset, RulesetAttr as _,
    RulesetCreatedAttr as _, RulesetStatus,
};
use rustix::fs::{Gid, Mode, OFlags, Uid};
use std::os::fd::OwnedFd;

fn err(e: impl std::fmt::Display) -> ConfineError {
    ConfineError(e.to_string())
}

/// The directory `path`, to serve or to name in a rule.
pub fn dir(path: &str) -> Result<OwnedFd, ConfineError> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::open(path, flags, Mode::empty()).map_err(|e| err(format!("{path}: {e}")))
}

/// Becomes the user namespace's root: the workload's root, on the host its first id.
fn become_root() -> Result<(), ConfineError> {
    let (gid, uid) = (Gid::from_raw(0), Uid::from_raw(0));
    rustix::thread::set_thread_res_gid(gid, gid, gid).map_err(|e| err(format!("gid: {e}")))?;
    rustix::thread::set_thread_res_uid(uid, uid, uid).map_err(|e| err(format!("uid: {e}")))?;
    rustix::process::chdir("/").map_err(|e| err(format!("the root: {e}")))
}

/// Applies Landlock: each of `volumes`, all of it when `write` and its reading otherwise, the
/// root's listing, and no connection; whether the kernel enforces it.
fn landlock(volumes: &[&str], write: bool) -> Result<bool, ConfineError> {
    let abi = ABI::V5;
    let all = AccessFs::from_all(abi);
    let given = if write { all } else { AccessFs::from_read(abi) };
    let mut created = Ruleset::default()
        .handle_access(all)
        .and_then(|r| r.handle_access(AccessNet::ConnectTcp | AccessNet::BindTcp))
        .and_then(Ruleset::create)
        .map_err(err)?
        .add_rule(PathBeneath::new(dir("/")?, AccessFs::ReadDir))
        .map_err(err)?;
    for v in volumes {
        created = created.add_rule(PathBeneath::new(dir(v)?, given)).map_err(err)?;
    }
    let status = created.restrict_self().map_err(err)?;
    Ok(status.ruleset != RulesetStatus::NotEnforced)
}

/// What the worker holds once confined, for its log.
#[derive(Debug)]
pub struct Held {
    pub landlock: bool,
    pub capabilities: Vec<String>,
}

/// Confines the worker serving `volumes`, for writing when `write`.
pub fn confine(volumes: &[&str], write: bool) -> Result<Held, ConfineError> {
    become_root()?;
    let landlock = landlock(volumes, write)?;
    let keep: &[_] = if write { &[CHOWN, FOWNER, DAC_OVERRIDE] } else { &[DAC_READ_SEARCH] };
    let capabilities = korpis_confine::keep_capabilities(keep)?;
    korpis_confine::no_new_privileges()?;
    Ok(Held { landlock, capabilities })
}
