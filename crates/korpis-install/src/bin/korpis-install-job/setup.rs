// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the first process, still root, builds for the worker once the worker has made its own
//! user, mount, network, and IPC namespaces: the worker's ids mapped to its workload's, and its
//! root. The root is an empty, read-only tmpfs holding each of the workload's volumes at the path
//! the workload sees it at, each an idmapped mount through the worker's user namespace, so the
//! worker, root only there, writes each file as the workload's id it names; and the node's content,
//! read-only, at `/.korpis/content`. Nothing of the host is left in it, not even `/proc`.
//!
//! A volume mounted inside another is mounted on a directory made in the outer volume, which the
//! tenant writes, so that directory is made and reached with no link followed, and mounted on
//! through the descriptor it was reached by rather than by its path.

use korpis_proto::korpis::install::v1 as pb;
use rustix::fs::{Mode, OFlags, ResolveFlags, mkdirat, openat2};
use rustix::io::Errno;
use rustix::mount::{
    MountFlags, MountPropagationFlags, MoveMountFlags, OpenTreeFlags, UnmountFlags,
};
use rustix::thread::LinkNameSpaceType;
use std::os::fd::{AsRawFd as _, OwnedFd};

/// Where the content store is, in the worker's root.
pub const CONTENT: &str = "/.korpis/content";

/// How a path beneath a root is reached: no step out of it, no link of any kind.
const WALK: ResolveFlags =
    ResolveFlags::BENEATH.union(ResolveFlags::NO_SYMLINKS).union(ResolveFlags::NO_MAGICLINKS);

const DIR: OFlags = OFlags::PATH.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC);

fn err(what: &str) -> impl Fn(Errno) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// The directory `rel` beneath `root`, each missing component made (0755) when `make`, no link
/// followed.
fn walk(root: &OwnedFd, rel: &str, make: bool) -> Result<OwnedFd, String> {
    let mut at = openat2(root, ".", DIR, Mode::empty(), WALK).map_err(err("the root"))?;
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        if make {
            match mkdirat(&at, part, Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(e) => return Err(format!("{rel}: {e}")),
            }
        }
        at = openat2(&at, part, DIR, Mode::empty(), WALK).map_err(err(rel))?;
    }
    Ok(at)
}

/// Sets `attributes` on the detached mount `tree`, and its id mapping to `userns`'s when given,
/// and makes it private. A copy of a mount keeps the original's propagation, and the host's mounts
/// are shared, so a mount made on a copy left shared, as a nested volume is mounted on the outer
/// one, would propagate back into the host's mount namespace.
fn set_attributes(tree: &OwnedFd, attributes: u64, userns: Option<&OwnedFd>) -> Result<(), String> {
    let attr = libc::mount_attr {
        attr_set: attributes | userns.map_or(0, |_| libc::MOUNT_ATTR_IDMAP),
        attr_clr: 0,
        propagation: libc::MS_PRIVATE,
        userns_fd: userns.map_or(0, |u| u64::try_from(u.as_raw_fd()).unwrap_or(0)),
    };
    // SAFETY: `tree` is an open descriptor for the whole call; the path is a NUL-terminated empty
    // string, as AT_EMPTY_PATH asks; `attr` is a live `mount_attr` and its size is passed with it.
    #[allow(unsafe_code, reason = "mount_setattr has no safe binding")]
    let r = unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            tree.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            &raw const attr,
            std::mem::size_of::<libc::mount_attr>(),
        )
    };
    if r == 0 { Ok(()) } else { Err(format!("mount_setattr: {}", std::io::Error::last_os_error())) }
}

/// A detached copy of the mount of `dir`, with `attributes`, idmapped through `userns` if given.
fn tree(dir: &str, attributes: u64, userns: Option<&OwnedFd>) -> Result<OwnedFd, String> {
    let src =
        rustix::fs::open(dir, DIR.union(OFlags::NOFOLLOW), Mode::empty()).map_err(err(dir))?;
    let flags = OpenTreeFlags::OPEN_TREE_CLONE | OpenTreeFlags::OPEN_TREE_CLOEXEC;
    let tree = rustix::mount::open_tree(&src, "", flags | OpenTreeFlags::AT_EMPTY_PATH)
        .map_err(err(dir))?;
    set_attributes(&tree, attributes, userns).map_err(|e| format!("{dir}: {e}"))?;
    Ok(tree)
}

/// Attaches `tree` on the directory `at`.
fn attach(tree: &OwnedFd, at: &OwnedFd, what: &str) -> Result<(), String> {
    let flags = MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH | MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH;
    rustix::mount::move_mount(tree, "", at, "", flags).map_err(err(what))
}

/// Maps the worker `pid`'s ids, 0 to 65535, to the workload's, from `base`.
fn map_ids(pid: u32, base: u32) -> Result<(), String> {
    let map = format!("0 {base} 65536\n");
    for file in ["uid_map", "gid_map"] {
        std::fs::write(format!("/proc/{pid}/{file}"), &map).map_err(|e| format!("{file}: {e}"))?;
    }
    Ok(())
}

/// The volume each volume of `spec` is mounted inside, if any: the one at the longest path that
/// is a proper prefix of its own.
fn outer<'s>(spec: &'s pb::InstallSpec, v: &pb::InstallVolume) -> Option<&'s pb::InstallVolume> {
    spec.volumes
        .iter()
        .filter(|o| o.path != v.path && v.path.starts_with(&format!("{}/", o.path)))
        .max_by_key(|o| o.path.len())
}

/// Builds the worker `pid`'s ids and root, and leaves the worker and this process in it.
///
/// # Errors
/// A map, a mount, or the move into the root failed.
pub fn build(spec: &pb::InstallSpec, pid: u32) -> Result<(), String> {
    map_ids(pid, spec.id_base)?;
    let ns = |kind: &str| {
        let path = format!("/proc/{pid}/ns/{kind}");
        rustix::fs::open(path.as_str(), OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
            .map_err(err("the worker's namespaces"))
    };
    let (userns, mntns) = (ns("user")?, ns("mnt")?);
    let shut = libc::MOUNT_ATTR_NOSUID | libc::MOUNT_ATTR_NODEV | libc::MOUNT_ATTR_NOEXEC;
    // A volume inside another needs a directory in the outer one to be mounted on, made now, in
    // the host's view of the outer volume, as the workload's root: the worker's root cannot make
    // it through the idmapped mount, which maps no id of the host's.
    let mut trees = Vec::new();
    for v in &spec.volumes {
        if let Some(o) = outer(spec, v) {
            let host =
                rustix::fs::open(o.directory.as_str(), DIR, Mode::empty()).map_err(err(&o.path))?;
            walk(&host, v.path.strip_prefix(o.path.as_str()).unwrap_or_default(), true)?;
        }
        trees.push((v, tree(&v.directory, shut, Some(&userns))?));
    }
    let content = if spec.content.is_empty() {
        None
    } else {
        Some(tree(&spec.content, shut | libc::MOUNT_ATTR_RDONLY, None)?)
    };
    rustix::thread::move_into_link_name_space(
        rustix::fd::AsFd::as_fd(&mntns),
        Some(LinkNameSpaceType::Mount),
    )
    .map_err(err("the worker's mount namespace"))?;
    root(spec, &mut trees, content.as_ref())
}

/// In the worker's mount namespace: its root made, and both processes moved into it.
fn root(
    spec: &pb::InstallSpec,
    trees: &mut [(&pb::InstallVolume, OwnedFd)],
    content: Option<&OwnedFd>,
) -> Result<(), String> {
    let private = MountPropagationFlags::REC | MountPropagationFlags::PRIVATE;
    rustix::mount::mount_change("/", private).map_err(err("making mounts private"))?;
    let shut = MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC;
    let root = spec.root.as_str();
    rustix::mount::mount("tmpfs", root, "tmpfs", shut, c"mode=0755,size=1m,nr_inodes=4096")
        .map_err(err("the root"))?;
    let root_fd = rustix::fs::open(root, DIR.union(OFlags::NOFOLLOW), Mode::empty())
        .map_err(err("the root"))?;
    if let Some(tree) = content {
        attach(tree, &walk(&root_fd, CONTENT, true)?, "the content")?;
    }
    trees.sort_by_key(|(v, _)| v.path.matches('/').count());
    for (v, tree) in trees.iter() {
        // Outermost first: a top-level path is made in the tmpfs; a nested one was made already.
        let top = !spec.volumes.iter().any(|o| v.path.starts_with(&format!("{}/", o.path)));
        attach(tree, &walk(&root_fd, &v.path, top)?, &v.path)?;
    }
    rustix::mount::mount_remount(root, MountFlags::RDONLY | shut, "mode=0755")
        .map_err(err("the root, read-only"))?;
    rustix::process::fchdir(&root_fd).map_err(err("entering the root"))?;
    rustix::process::pivot_root(".", ".").map_err(err("pivot_root"))?;
    rustix::mount::unmount(".", UnmountFlags::DETACH).map_err(err("leaving the host's root"))?;
    rustix::process::chdir("/").map_err(err("the root"))
}
