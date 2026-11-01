// SPDX-License-Identifier: AGPL-3.0-or-later

//! The view a tree's receiving rsync runs in (§8.1 of docs/05-scheduling.md, Finding 218): a
//! mount namespace of its own whose root is a fresh `tmpfs` holding, read only, the host's
//! programs and libraries (`/usr` and what `/bin`, `/lib`, `/lib64`, and `/sbin` are, links kept
//! as links) and the loader's cache, the helper's own binary, which rsync runs as its remote
//! shell, and, writable, the destination's copy of the volume at its own path. Nothing else of
//! the host is in it: no `/etc` beyond the loader's cache, no `/proc`, no other volume, so a
//! stream that misleads rsync's receiver can write only into the copy it was pulling into.
//!
//! A receiver taken over by a hostile stream runs whatever code the stream gives it. As root with
//! every capability it would leave a mount namespace easily, mounting `proc` and walking
//! `/proc/1/root` to the host's root, and as root at all it could signal every root process of
//! the host. So the receiver is not root: it runs as a user of its own that no workload or host
//! user is, keeping only the capabilities writing owners, modes, devices and extended attributes
//! needs ([`KEPT`], which rsync uses under `--super`), none of them regainable, with
//! `no_new_privs`, and with a network, IPC and host name of its own: it talks to the source only
//! through the descriptor its remote shell inherited.
//!
//! This is one of the crate's unsafe modules, listed in the crate's header: the view is made in
//! the child between fork and exec, which only `pre_exec` reaches, and there only calls that
//! neither allocate nor lock may run. Everything it needs is computed before, in the helper.

#![expect(unsafe_code, reason = "the view is made between fork and exec, by pre_exec")]

use korpis_core::workload::assignment::{FIRST_WORKLOAD_ID, IDS_PER_WORKLOAD, IdRange};
use rustix::fs::{Mode, OFlags};
use rustix::mount::{MountFlags, MountPropagationFlags, UnmountFlags};
use rustix::process::{Gid, Uid};
use rustix::thread::{CapabilitySet, CapabilitySets, UnshareFlags};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the view's root is made; a directory on the host, which the child's own `tmpfs` hides.
const ROOT: &str = "/run/korpis-helper/confined";
/// The host's top-level directories the view holds, read only, or links to as the host does.
const SYSTEM: [&str; 5] = ["usr", "bin", "lib", "lib64", "sbin"];
/// The loader's cache: without it the libraries in multiarch directories are not found.
const LOADER_CACHE: &str = "etc/ld.so.cache";
/// What `rsync -aHAXS --numeric-ids` needs as root to write what it receives: owners by number,
/// set-user-ID bits, device files (which the copy's `nodev` mount leaves unopenable), and file
/// capabilities among the extended attributes. Not `CAP_SYS_ADMIN`, without which nothing in the
/// view mounts, unshares or pivots again.
const KEPT: CapabilitySet = CapabilitySet::CHOWN
    .union(CapabilitySet::DAC_OVERRIDE)
    .union(CapabilitySet::FOWNER)
    .union(CapabilitySet::FSETID)
    .union(CapabilitySet::MKNOD)
    .union(CapabilitySet::SETFCAP);

/// What the child does, in order, every path already a C string.
#[derive(Debug, Default)]
struct Plan {
    root: CString,
    old: CString,
    /// Directories made in the view, parents first.
    dirs: Vec<CString>,
    /// Empty files made in the view for a file to be bound onto.
    files: Vec<CString>,
    /// Links made in the view: its target, then where.
    links: Vec<(CString, CString)>,
    /// Bindings: the host's path, the view's, and whether it is writable.
    binds: Vec<(CString, CString, bool)>,
}

fn c(path: &Path) -> std::io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(std::io::Error::other)
}

impl Plan {
    /// Makes, in the view, every directory from its root down to `dir`, if not already made.
    fn ancestors(&mut self, root: &Path, dir: &Path) -> std::io::Result<()> {
        let mut at = root.to_path_buf();
        for part in dir.strip_prefix("/").unwrap_or(dir).components() {
            at.push(part);
            let path = c(&at)?;
            if !self.dirs.contains(&path) {
                self.dirs.push(path);
            }
        }
        Ok(())
    }

    /// The plan for a view holding `copy`, writable, and `relay`, read only.
    fn new(copy: &Path, relay: &Path) -> std::io::Result<Self> {
        let root = PathBuf::from(ROOT);
        let mut plan = Self { root: c(&root)?, old: c(&root.join(".old"))?, ..Self::default() };
        plan.dirs.push(plan.old.clone());
        for name in SYSTEM {
            let host = Path::new("/").join(name);
            let Ok(meta) = std::fs::symlink_metadata(&host) else { continue };
            if meta.file_type().is_symlink() {
                plan.links.push((c(&std::fs::read_link(&host)?)?, c(&root.join(name))?));
            } else if meta.is_dir() {
                plan.dirs.push(c(&root.join(name))?);
                plan.binds.push((c(&host)?, c(&root.join(name))?, false));
            }
        }
        let cache = Path::new("/").join(LOADER_CACHE);
        if cache.is_file() {
            plan.ancestors(&root, Path::new("/etc"))?;
            plan.files.push(c(&root.join(LOADER_CACHE))?);
            plan.binds.push((c(&cache)?, c(&root.join(LOADER_CACHE))?, false));
        }
        let under_system = |p: &Path| SYSTEM.iter().any(|s| p.starts_with(Path::new("/").join(s)));
        if !under_system(relay) {
            let inside = root.join(relay.strip_prefix("/").unwrap_or(relay));
            plan.ancestors(&root, relay.parent().unwrap_or(Path::new("/")))?;
            plan.files.push(c(&inside)?);
            plan.binds.push((c(relay)?, c(&inside)?, false));
        }
        plan.ancestors(&root, copy)?;
        let inside = root.join(copy.strip_prefix("/").unwrap_or(copy));
        plan.binds.push((c(copy)?, c(&inside)?, true));
        Ok(plan)
    }

    /// Makes the view and enters it; runs in the child, between fork and exec.
    fn enter(&self) -> rustix::io::Result<()> {
        use rustix::mount::{mount, mount_bind, mount_change, mount_remount, unmount};
        let own = UnshareFlags::NEWNS | UnshareFlags::NEWNET | UnshareFlags::NEWIPC;
        // SAFETY: called from `pre_exec` alone, in the single-threaded child, where this call
        // allocates nothing and changes only the child's own namespaces.
        unsafe { rustix::thread::unshare_unsafe(own | UnshareFlags::NEWUTS)? };
        mount_change(c"/", MountPropagationFlags::REC | MountPropagationFlags::PRIVATE)?;
        let hardened = MountFlags::NOSUID | MountFlags::NODEV;
        mount(c"tmpfs", self.root.as_c_str(), c"tmpfs", hardened, c"mode=0755")?;
        for dir in &self.dirs {
            rustix::fs::mkdir(dir.as_c_str(), Mode::from_raw_mode(0o755))?;
        }
        for file in &self.files {
            let flags = OFlags::CREATE | OFlags::WRONLY | OFlags::CLOEXEC;
            drop(rustix::fs::open(file.as_c_str(), flags, Mode::from_raw_mode(0o644))?);
        }
        for (target, at) in &self.links {
            rustix::fs::symlink(target.as_c_str(), at.as_c_str())?;
        }
        for (host, inside, writable) in &self.binds {
            mount_bind(host.as_c_str(), inside.as_c_str())?;
            let mut flags = MountFlags::BIND | hardened;
            if !writable {
                flags |= MountFlags::RDONLY;
            }
            mount_remount(inside.as_c_str(), flags, c"")?;
        }
        rustix::process::pivot_root(self.root.as_c_str(), self.old.as_c_str())?;
        rustix::process::chdir(c"/")?;
        unmount(c"/.old", UnmountFlags::DETACH)?;
        become_receiver(KEPT)
    }
}

/// The first of the ids a receiver runs as: the last range of [`IDS_PER_WORKLOAD`] ids above
/// [`FIRST_WORKLOAD_ID`], which reaches `u32::MAX` and so is never a workload's, and which no
/// host user takes. A receiver is one of the 65,534 below `u32::MAX - 1`, by its process id, so
/// two running at once are almost never the same user and cannot signal each other.
fn receiver_ids() -> (u32, u32) {
    let first = FIRST_WORKLOAD_ID + IdRange::slots() * IDS_PER_WORKLOAD;
    (first, u32::MAX - 1 - first)
}

/// Gives up root: the child becomes a user of its own ([`receiver_ids`]) with no groups that
/// keeps `kept`, as ambient capabilities, through its exec, and nothing else, with root's
/// special treatment locked off and `no_new_privs` set. A user that is not root signals no
/// process of the host's, and none of the host's users are it. Allocates nothing, so it may run
/// between fork and exec.
fn become_receiver(kept: CapabilitySet) -> rustix::io::Result<()> {
    use rustix::thread::CapabilitiesSecureBits as Bits;
    let (first, count) = receiver_ids();
    let id = first + rustix::process::getpid().as_raw_nonzero().get().unsigned_abs() % count;
    rustix::thread::set_capabilities_secure_bits(
        Bits::NO_ROOT
            | Bits::NO_ROOT_LOCKED
            | Bits::NO_SETUID_FIXUP
            | Bits::NO_SETUID_FIXUP_LOCKED
            | Bits::KEEP_CAPS_LOCKED,
    )?;
    drop_bounding(kept)?;
    rustix::thread::set_thread_groups(&[])?;
    let (uid, gid) = (Uid::from_raw_unchecked(id), Gid::from_raw_unchecked(id));
    rustix::thread::set_thread_res_gid(gid, gid, gid)?;
    rustix::thread::set_thread_res_uid(uid, uid, uid)?;
    let sets = CapabilitySets { effective: kept, permitted: kept, inheritable: kept };
    rustix::thread::set_capabilities(None, sets)?;
    for bit in 0..64u32 {
        let cap = CapabilitySet::from_bits_retain(1u64 << bit);
        if kept.contains(cap) {
            rustix::thread::configure_capability_in_ambient_set(cap, true)?;
        }
    }
    rustix::thread::set_no_new_privs(true)
}

/// Drops every capability but `kept` from the bounding set, so nothing executed regains one.
fn drop_bounding(kept: CapabilitySet) -> rustix::io::Result<()> {
    for bit in 0..64u32 {
        let cap = CapabilitySet::from_bits_retain(1u64 << bit);
        if kept.contains(cap) {
            continue;
        }
        match rustix::thread::remove_capability_from_bounding_set(cap) {
            // A number the kernel has no capability for.
            Ok(()) | Err(rustix::io::Errno::INVAL) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Makes `command` run in a view holding only the host's programs and libraries, `relay`, and
/// `copy`, writable, at their own paths.
///
/// # Errors
///
/// A path that is not one, or the view's root that cannot be made.
pub fn confine(command: &mut Command, copy: &Path, relay: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(ROOT)?;
    let plan = Plan::new(copy, relay)?;
    // SAFETY: the closure runs in the child between fork and exec; `enter` allocates nothing,
    // takes no lock, and only reads the plan, which was made before the fork.
    unsafe { command.pre_exec(move || plan.enter().map_err(std::io::Error::from)) };
    Ok(())
}
