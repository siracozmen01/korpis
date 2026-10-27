// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class's device given to a guest as its disk (§2.1 and §3.1 of docs/06-storage.md).
//! What a guest writes on it is parsed by the guest's kernel alone, so the host never mounts,
//! probes, or reads it: the helper formats it ext4 only while no volume was made of it, which
//! writes and reads nothing a guest wrote, and gives the driver a device node of it where a block
//! device's image would be, in a volume directory marked as a block device's. A device made for a
//! guest is never mounted for a volume of files, nor one made for files given to a guest.

use super::Class;
use super::device::Devices;
use crate::block::{IMAGE, mark};
use crate::error::{HelperError, HelperResult};
use korpis_proto::korpis::helper::v1 as helper;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::Path;
use std::process::{Command, Stdio};

/// That a device made as `guest` is asked for a volume of the same kind.
pub(super) fn same_kind(volume: &str, guest: bool, block_device: bool) -> HelperResult<()> {
    match (guest, block_device) {
        (true, false) => Err(HelperError::Refused(format!(
            "volume {volume}'s device is a guest's disk, which the host never mounts"
        ))),
        (false, true) => Err(HelperError::Refused(format!(
            "volume {volume}'s device holds a filesystem the host mounts, and is not a guest's"
        ))),
        _ => Ok(()),
    }
}

/// Formats the guest's disk `device` empty, its root owned by id 0 as the guest sees it.
fn format(volume: &str, device: &Path) -> HelperResult<()> {
    let made = Command::new("mke2fs")
        .args(["-q", "-F", "-t", crate::block::FILESYSTEM, "-m", "0", "-E", "root_owner=0:0"])
        .arg(device)
        .stdin(Stdio::null())
        .output()?;
    if !made.status.success() {
        let said = String::from_utf8_lossy(&made.stderr).into_owned();
        return Err(HelperError::Failed(format!("formatting volume {volume}: {said}")));
    }
    Ok(())
}

impl Class {
    /// Attaches the guest's disk `asked` names for `r`'s volume, formats it while no volume was
    /// made of it, and puts a device node of it in the volume's directory; the node.
    pub(super) fn guest_disk(
        &self,
        devices: &Devices,
        r: &helper::EnsureVolumeRequest,
        asked: &helper::BlockDevice,
    ) -> HelperResult<helper::EnsureVolumeResponse> {
        let named = (r.volume.as_str(), r.workload.as_str());
        let device = devices.attach(named, &asked.target, &asked.credential)?;
        let node = self.node_of(&r.volume, &device).inspect_err(|_| self.let_go(&r.volume))?;
        if asked.blank {
            format(&r.volume, &device).inspect_err(|_| self.let_go(&r.volume))?;
        }
        Ok(helper::EnsureVolumeResponse {
            host_path: node.to_string_lossy().into_owned(),
            created: asked.blank,
            quota_enforced: true,
            storage_class: self.name().to_owned(),
            ..Default::default()
        })
    }

    /// The device node of `device` in the volume `volume`'s directory, marked as a block
    /// device's, made or made again when it names another device.
    fn node_of(&self, volume: &str, device: &Path) -> HelperResult<std::path::PathBuf> {
        use rustix::fs::{AtFlags, FileType, Mode};
        let rdev = std::fs::metadata(device)?;
        if !rdev.file_type().is_block_device() {
            return Err(HelperError::Failed(format!("{} is not a block device", device.display())));
        }
        match rustix::fs::mkdirat(&self.handle, volume, Mode::from_raw_mode(0o711)) {
            Ok(()) => mark(&self.path_of(volume))?,
            Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(std::io::Error::from(e).into()),
        }
        let dir = std::fs::File::open(self.path_of(volume))?;
        let node = self.path_of(volume).join(IMAGE);
        match std::fs::symlink_metadata(&node) {
            Ok(m) if m.file_type().is_block_device() && m.rdev() == rdev.rdev() => return Ok(node),
            Ok(_) => {
                rustix::fs::unlinkat(&dir, IMAGE, AtFlags::empty())
                    .map_err(std::io::Error::from)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mode = Mode::from_raw_mode(0o600);
        rustix::fs::mknodat(&dir, IMAGE, FileType::BlockDevice, mode, rdev.rdev())
            .map_err(std::io::Error::from)?;
        Ok(node)
    }
}
