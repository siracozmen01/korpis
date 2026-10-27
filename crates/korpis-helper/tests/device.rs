// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class's volume (§2.1 of docs/06-storage.md), its device a file in a directory attached
//! through a loop device, as a provider of the tests' own answers: attached, formatted ext4 when
//! it holds nothing, and mounted as the volume; found when asked again; released, the loop device
//! cleared and the file kept; and attached again holding what was written, as on the node a
//! workload fails over to. A target naming anything but a file in the directory, and a device
//! holding anything but ext4, are refused. A device made for a guest is formatted while blank,
//! never mounted, and given as a device node in a directory marked as a block device's; asked
//! for a volume of files it is refused. It needs root, a loop device, and the e2fsprogs, and is
//! skipped without them.

#![allow(clippy::unwrap_used, clippy::panic, reason = "a failing test should fail loudly")]

use korpis_helper::storage::Class;
use korpis_helper::storage::device::Protocol;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::process::Command;

const MIB: u64 = 1 << 20;

fn attach(volume: &str, target: &str) -> helper::EnsureVolumeRequest {
    helper::EnsureVolumeRequest {
        volume: volume.into(),
        storage_class: "fast".into(),
        device: helper::BlockDevice {
            protocol: pb::BlockProtocol::BLOCK_PROTOCOL_LOOP.into(),
            target: target.into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// Whether any loop device is backed by `file`, once a clear the kernel deferred while udev held
/// the device open has had five seconds to happen.
fn looped(file: &Path) -> bool {
    let any = || {
        std::fs::read_dir("/sys/block").unwrap().filter_map(Result::ok).any(|e| {
            let backing = e.path().join("loop/backing_file");
            std::fs::read_to_string(backing).is_ok_and(|b| Path::new(b.trim_end()) == file)
        })
    };
    for _ in 0..50 {
        if !any() {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    true
}

fn usable() -> bool {
    let root = std::fs::metadata("/proc/self").unwrap().uid() == 0;
    let tools =
        ["mke2fs", "blkid", "mkswap"].iter().all(|t| Command::new(t).arg("-V").output().is_ok());
    root && tools && Path::new("/dev/loop-control").exists()
}

#[test]
fn a_device_is_attached_formatted_mounted_released_and_attached_again_holding_its_data() {
    if !usable() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let disks = dir.path().join("disks");
    std::fs::create_dir(&disks).unwrap();
    let image = disks.join("vol-1.img");
    std::fs::File::create(&image).unwrap().set_len(64 * MIB).unwrap();
    let state = dir.path().join("state");
    let protocol = Protocol::Loop(disks.clone());
    let mut class =
        Class::block("fast", &state.join("block"), protocol, &state.join("devices")).unwrap();
    let described = class.describe();
    assert_eq!(described.block_protocol.as_known(), Some(pb::BlockProtocol::BLOCK_PROTOCOL_LOOP));
    assert!(described.capabilities.as_option().unwrap().multi_node_attach);

    let made = class.ensure(&attach("wl_d.data", "vol-1.img")).unwrap();
    assert!(made.created && made.quota_enforced, "formatted, its size its bound: {made:?}");
    let mounted = Path::new(&made.host_path).to_owned();
    std::fs::write(mounted.join("world"), b"spawn").unwrap();
    let again = class.ensure(&attach("wl_d.data", "vol-1.img")).unwrap();
    assert!(!again.created && again.host_path == made.host_path, "found mounted");
    let usage = class.usage("wl_d.data").unwrap();
    assert!(usage.accounted && usage.limit_bytes > 0 && usage.limit_bytes <= 64 * MIB);

    assert!(class.delete("wl_d.data").unwrap(), "let go of");
    assert!(!mounted.exists() && !looped(&image), "unmounted, and the loop device cleared");
    assert!(image.exists(), "the storage keeps its bytes");
    let back = class.ensure(&attach("wl_d.data", "vol-1.img")).unwrap();
    assert!(!back.created, "ext4 already: mounted as it is");
    assert_eq!(std::fs::read(mounted.join("world")).unwrap(), b"spawn");
    assert!(class.delete("wl_d.data").unwrap());

    for bad in ["../vol-1.img", "a/vol-1.img", ".vol", ""] {
        assert!(class.ensure(&attach("wl_d.bad", bad)).is_err(), "{bad:?}");
    }
    let swap = disks.join("swap.img");
    std::fs::File::create(&swap).unwrap().set_len(16 * MIB).unwrap();
    assert!(Command::new("mkswap").arg(&swap).output().unwrap().status.success());
    let refused = class.ensure(&attach("wl_d.swap", "swap.img")).unwrap_err().to_string();
    assert!(refused.contains("never mounts"), "{refused}");
    class.delete("wl_d.swap").unwrap();
    assert!(!looped(&swap), "a refused device is let go of");
}

#[test]
fn a_guest_s_disk_is_formatted_while_blank_given_as_a_device_node_and_never_mounted() {
    use std::os::unix::fs::FileTypeExt as _;
    if !usable() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let disks = dir.path().join("disks");
    std::fs::create_dir(&disks).unwrap();
    let image = disks.join("vol-2.img");
    std::fs::File::create(&image).unwrap().set_len(32 * MIB).unwrap();
    let state = dir.path().join("state");
    let protocol = Protocol::Loop(disks.clone());
    let mut class =
        Class::block("fast", &state.join("block"), protocol, &state.join("devices")).unwrap();
    let guest = |blank| {
        let mut r = attach("wl_g.disk", "vol-2.img");
        r.block_device = true;
        let d = r.device.as_option_mut().unwrap();
        (d.guest, d.blank) = (true, blank);
        r
    };
    let made = class.ensure(&guest(true)).unwrap();
    let node = Path::new(&made.host_path).to_owned();
    assert!(std::fs::symlink_metadata(&node).unwrap().file_type().is_block_device());
    assert!(korpis_helper::block::is_block(node.parent().unwrap()), "marked as a block device's");
    let probed = Command::new("blkid").args(["-o", "value", "-s", "TYPE"]).arg(&image).output();
    assert_eq!(String::from_utf8_lossy(&probed.unwrap().stdout).trim(), "ext4", "formatted");
    let again = class.ensure(&guest(false)).unwrap();
    assert_eq!(again.host_path, made.host_path, "found as it is");
    let mut files = attach("wl_g.disk", "vol-2.img");
    files.device.as_option_mut().unwrap().guest = true;
    let refused = class.ensure(&files).unwrap_err().to_string();
    assert!(refused.contains("never mounts"), "{refused}");
    assert!(class.delete("wl_g.disk").unwrap());
    assert!(!node.exists() && !looped(&image), "the node removed, the loop device cleared");
}
