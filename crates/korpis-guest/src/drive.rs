// SPDX-License-Identifier: AGPL-3.0-or-later

//! The drives of a microVM, in the order its driver attaches them, so each has a name the guest
//! can count on: the boot drive holding `korpis-guest`, which the kernel mounts as its root; the
//! workload's image, read-only, or a service guest's content drive in its place; the config
//! drive; then each volume in the order the config lists them.

/// The boot drive: `korpis-guest` and the directories it mounts on, shared by every guest of a
/// node and mounted read-only.
pub const BOOT: &str = "/dev/vda";

/// The workload's image, its root filesystem's lower layer, read-only.
pub const IMAGE: &str = "/dev/vdb";

/// The config drive (`crate::config`), which also carries back how the run ended.
pub const CONFIG: &str = "/dev/vdc";

/// On a service guest that runs an install, the image's drive is the install's content drive,
/// and this is the file on it holding the install's spec (`korpis_image::content::SPEC`).
pub const INSTALL_SPEC: &str = "spec";

/// Where a service guest mounts an install's content drive, beneath its root's `/run`; the
/// install's spec names it as the job's content.
pub const INSTALL_CONTENT: &str = "/run/.korpis-content";

/// Where a restore's backup image, one of the guest's volumes, is mounted read-only: at the
/// service root's top, since `/run` is mounted over once the volumes' mount points are made.
pub const RESTORE_IMAGE: &str = "/.korpis-backup";

/// The install job's root, which it mounts its own on, and its output, where it writes its
/// progress and result, in a service guest's `/run`.
pub const INSTALL_ROOT: &str = "/run/.korpis-install/root";
/// See [`INSTALL_ROOT`].
pub const INSTALL_OUT: &str = "/run/.korpis-install/out";

/// How many drives come before the first volume.
const BEFORE_VOLUMES: usize = 3;

/// The name the guest's kernel gives the `index`th drive, counting from zero: `vda` to `vdz`,
/// then `vdaa`, as virtio block devices are named.
#[must_use]
pub fn name(index: usize) -> String {
    let mut letters = Vec::new();
    let mut n = index + 1;
    while n > 0 {
        let digit = u8::try_from((n - 1) % 26).unwrap_or(0);
        letters.push(char::from(b'a' + digit));
        n = (n - 1) / 26;
    }
    letters.iter().rev().fold(String::from("vd"), |mut s, c| {
        s.push(*c);
        s
    })
}

/// The device of the `index`th volume, counting from zero.
#[must_use]
pub fn volume(index: usize) -> String {
    format!("/dev/{}", name(index + BEFORE_VOLUMES))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_are_named_as_the_guest_s_kernel_names_them() {
        assert_eq!([name(0), name(1), name(2)], ["vda", "vdb", "vdc"]);
        assert_eq!((name(25), name(26), name(27)), ("vdz".into(), "vdaa".into(), "vdab".into()));
        assert_eq!(format!("/dev/{}", name(2)), CONFIG);
        assert_eq!((volume(0), volume(1)), ("/dev/vdd".into(), "/dev/vde".into()));
    }
}
