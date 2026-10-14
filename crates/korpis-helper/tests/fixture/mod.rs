// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the helper's tests that need a real filesystem share: a btrfs filesystem made in a file
//! and mounted from a loop device.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A btrfs filesystem, mounted until dropped.
pub struct Mounted(pub PathBuf);

impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = Command::new("umount").arg(&self.0).status();
    }
}

/// A btrfs filesystem in `dir`, with qgroups on if `quota`; `None` where one cannot be made.
pub fn mounted(dir: &Path, quota: bool) -> Option<Mounted> {
    let image = dir.join("fs.img");
    std::fs::File::create(&image).unwrap().set_len(256 << 20).unwrap();
    let made = Command::new("mkfs.btrfs").arg("-q").arg(&image).status().ok()?;
    let at = dir.join("mnt");
    std::fs::create_dir(&at).unwrap();
    let on = Command::new("mount").args(["-o", "loop"]).arg(&image).arg(&at).status().ok()?;
    if !(made.success() && on.success()) {
        return None;
    }
    let m = Mounted(at);
    if quota {
        let btrfs = |args: &[&str]| Command::new("btrfs").args(args).arg(&m.0).status();
        assert!(btrfs(&["quota", "enable"]).unwrap().success());
        assert!(btrfs(&["quota", "rescan", "-w"]).unwrap().success());
    }
    Some(m)
}
