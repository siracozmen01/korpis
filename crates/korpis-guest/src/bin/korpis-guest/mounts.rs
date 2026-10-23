// SPDX-License-Identifier: AGPL-3.0-or-later

//! The workload's root, assembled at `root` on the boot drive: the image drive read-only, a small
//! tmpfs above it in an overlay so the mount points it lacks can be made, then the overlay made
//! read-only, as a container's root is (§2 of docs/04-runtimes.md). Beneath it, the kernel's
//! filesystems, small tmpfs mounts at `/tmp`, `/run`, and `/dev/shm` so a read-only root is
//! livable, and the volumes, each its own drive's filesystem.

use crate::failed;
use korpis_guest::config::Config;
use korpis_guest::drive;
use nix::mount::{MsFlags, mount};
use std::path::{Component, Path, PathBuf};

/// Where the image is mounted, on the boot drive.
const LOWER: &str = "/lower";

/// The tmpfs holding the overlay's writable layer and its work directory.
const UPPER: &str = "/upper";

/// `path`, absolute in the workload's root, beneath `root`; refused if it is not absolute or
/// climbs out with `..`.
fn beneath(root: &str, path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    let plain = p.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    if !p.is_absolute() || !plain {
        return Err(format!("{path:?} is not an absolute path within the workload's root"));
    }
    Ok(Path::new(root).join(p.strip_prefix("/").unwrap_or(p)))
}

fn mounted(
    source: &str,
    target: &Path,
    fstype: &str,
    flags: MsFlags,
    data: Option<&str>,
) -> Result<(), String> {
    mount(Some(source), target, Some(fstype), flags, data)
        .map_err(|e| format!("mounting {fstype} at {}: {e}", target.display()))
}

/// The overlay of the image and a fresh tmpfs at `root`, with every directory the config
/// mounts on made, then read-only.
fn overlay(root: &str, config: &Config) -> Result<(), String> {
    mounted(drive::IMAGE, Path::new(LOWER), "ext4", MsFlags::MS_RDONLY, None)?;
    let tmpfs = MsFlags::MS_NOSUID | MsFlags::MS_NODEV;
    mounted("tmpfs", Path::new(UPPER), "tmpfs", tmpfs, Some("size=16m,mode=0755"))?;
    for d in ["data", "work"] {
        std::fs::create_dir(Path::new(UPPER).join(d)).map_err(failed("the overlay's layer"))?;
    }
    let layers = format!("lowerdir={LOWER},upperdir={UPPER}/data,workdir={UPPER}/work");
    mounted("overlay", Path::new(root), "overlay", MsFlags::empty(), Some(&layers))?;
    let fixed = ["/proc", "/sys", "/dev", "/run", "/tmp"];
    let volumes = config.volumes.iter().map(|v| v.path.as_str());
    let working = std::iter::once(config.working_dir.as_str()).filter(|w| !w.is_empty());
    for d in fixed.into_iter().chain(volumes).chain(working) {
        std::fs::create_dir_all(beneath(root, d)?).map_err(failed("a mount point"))?;
    }
    let readonly = MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY;
    mount(None::<&str>, root, None::<&str>, readonly, None::<&str>)
        .map_err(failed("making the root read-only"))
}

/// The kernel's filesystems and the small writable ones, beneath `root`.
fn system(root: &str) -> Result<(), String> {
    let hidden = MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC;
    let at = |p: &str| beneath(root, p);
    mounted("proc", &at("/proc")?, "proc", hidden, None)?;
    mounted("sysfs", &at("/sys")?, "sysfs", hidden | MsFlags::MS_RDONLY, None)?;
    mounted("devtmpfs", &at("/dev")?, "devtmpfs", MsFlags::MS_NOSUID, Some("mode=0755"))?;
    for d in ["/dev/pts", "/dev/shm"] {
        std::fs::create_dir_all(at(d)?).map_err(failed("a device directory"))?;
    }
    let pts = Some("newinstance,ptmxmode=0666,mode=0620");
    mounted("devpts", &at("/dev/pts")?, "devpts", MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC, pts)?;
    let small = MsFlags::MS_NOSUID | MsFlags::MS_NODEV;
    mounted("tmpfs", &at("/dev/shm")?, "tmpfs", small, Some("size=64m,mode=1777"))?;
    mounted("tmpfs", &at("/run")?, "tmpfs", small, Some("size=16m,mode=0755"))?;
    mounted("tmpfs", &at("/tmp")?, "tmpfs", small, Some("size=64m,mode=1777"))
}

/// The workload's root at `root`, with its volumes.
pub fn root(root: &str, config: &Config) -> Result<(), String> {
    overlay(root, config)?;
    system(root)?;
    for (index, volume) in config.volumes.iter().enumerate() {
        let mut flags = MsFlags::MS_NOSUID | MsFlags::MS_NODEV;
        if volume.read_only {
            flags |= MsFlags::MS_RDONLY;
        }
        let at = beneath(root, &volume.path)?;
        mounted(&drive::volume(index), &at, "ext4", flags, None)?;
        if !volume.read_only {
            // A volume given more than it held is grown now; one that cannot be grown is still
            // the volume it was, so the run goes on and the console says why.
            let sys = beneath(root, "/sys")?;
            if let Err(e) = crate::grow::grow(&sys, &drive::volume(index), &at) {
                crate::say(&e);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_stays_beneath_the_root() {
        assert_eq!(beneath("/newroot", "/data").unwrap(), Path::new("/newroot/data"));
        assert_eq!(beneath("/newroot", "/").unwrap(), Path::new("/newroot"));
        assert!(beneath("/newroot", "/data/../../etc").is_err(), "climbing out is refused");
        assert!(beneath("/newroot", "data").is_err(), "a relative path is refused");
    }
}
