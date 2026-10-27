// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class's volume on this node (§2.1 of docs/06-storage.md): a directory under the
//! class's root, on which the volume's device is mounted, `nosuid` and `nodev`, once it is
//! attached as its provider answered. A device holding nothing is formatted ext4 first, its root
//! owned by id 0 as the workload sees it, as any volume's is; one holding ext4 is mounted as it
//! is, which is what lets the volume follow its workload to another node; one holding anything
//! else is refused, never mounted. Only a host's kernel writes such a filesystem, since a device
//! is given to a guest only when it was made for one, and is then never mounted
//! ([`super::guest`]), so mounting it parses nothing a tenant wrote raw. Releasing it unmounts it
//! and lets go of the device, whose bytes the storage keeps.

use super::Class;
use crate::error::{HelperError, HelperResult, identifier};
use korpis_proto::korpis::helper::v1 as helper;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// The filesystem a device is formatted with and mounted as.
const FILESYSTEM: &str = "ext4";
/// `blkid`'s exit code when it finds nothing on the device.
const NOTHING_FOUND: i32 = 2;

/// What `device` holds, as `blkid` probes it; none when it holds nothing it knows.
fn probe(device: &Path) -> HelperResult<Option<String>> {
    let ran = Command::new("blkid")
        .args(["-p", "-o", "value", "-s", "TYPE"])
        .arg(device)
        .stdin(Stdio::null())
        .output()?;
    match ran.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&ran.stdout).trim().to_owned())),
        Some(NOTHING_FOUND) => Ok(None),
        _ => Err(HelperError::Failed(format!(
            "probing {}: {}",
            device.display(),
            String::from_utf8_lossy(&ran.stderr)
        ))),
    }
}

/// Formats `device` ext4 when it holds nothing, its root owned by id 0; whether it was. One
/// holding anything but ext4 is refused.
fn formatted(volume: &str, device: &Path) -> HelperResult<bool> {
    match probe(device)? {
        Some(found) if found == FILESYSTEM => Ok(false),
        Some(found) => Err(HelperError::Refused(format!(
            "volume {volume}'s device holds {found:?}, which the helper never mounts"
        ))),
        None => {
            let made = Command::new("mke2fs")
                .args(["-q", "-t", FILESYSTEM, "-m", "0", "-E", "root_owner=0:0"])
                .arg(device)
                .stdin(Stdio::null())
                .output()?;
            if !made.status.success() {
                let said = String::from_utf8_lossy(&made.stderr).into_owned();
                return Err(HelperError::Failed(format!("formatting volume {volume}: {said}")));
            }
            Ok(true)
        }
    }
}

impl Class {
    /// Whether something is mounted on the volume directory `path`, which is then on another
    /// device than the class's root.
    fn mounted(&self, path: &Path) -> HelperResult<bool> {
        let root = self.handle.metadata()?.dev();
        match std::fs::symlink_metadata(path) {
            Ok(m) => Ok(m.dev() != root),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Attaches the device `r` names for its volume, formats it when it holds nothing, and mounts
    /// it as the volume; mounted already, it is found.
    pub(crate) fn ensure_device(
        &self,
        r: &helper::EnsureVolumeRequest,
    ) -> HelperResult<helper::EnsureVolumeResponse> {
        identifier("volume", &r.volume)?;
        let Some(devices) = self.devices.as_ref() else {
            return Err(HelperError::Failed(format!("class {} has no devices", self.name())));
        };
        let asked = r.device.as_option().ok_or_else(|| {
            HelperError::Refused(format!("volume {} has no device attached yet", r.volume))
        })?;
        if asked.protocol.as_known() != Some(devices.protocol().to_pb()) {
            return Err(HelperError::Refused(format!("class {} attaches by another", self.name())));
        }
        super::guest::same_kind(&r.volume, asked.guest, r.block_device)?;
        if asked.guest {
            return self.guest_disk(devices, r, asked);
        }
        let path = self.path_of(&r.volume);
        let mut created = false;
        if !self.mounted(&path)? {
            let named = (r.volume.as_str(), r.workload.as_str());
            let device = devices.attach(named, &asked.target, &asked.credential)?;
            created = self.mount(&r.volume, &device).inspect_err(|_| self.let_go(&r.volume))?;
            tracing::info!(volume = %r.volume, device = %device.display(), created, "mounted");
        }
        Ok(helper::EnsureVolumeResponse {
            host_path: path.to_string_lossy().into_owned(),
            created,
            quota_enforced: true,
            storage_class: self.name().to_owned(),
            ..Default::default()
        })
    }

    /// Lets go of what was attached for `volume`, which was not made: nothing is left attached.
    pub(super) fn let_go(&self, volume: &str) {
        if let Err(e) = self.release_device(volume) {
            tracing::warn!(%volume, error = %e, "device not let go of");
        }
    }

    /// Formats `device` when it holds nothing and mounts it as the volume `volume`; whether it
    /// was formatted.
    fn mount(&self, volume: &str, device: &Path) -> HelperResult<bool> {
        use rustix::mount::{MountFlags, mount};
        match rustix::fs::mkdirat(&self.handle, volume, 0o700.into()) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(std::io::Error::from(e).into()),
        }
        let created = formatted(volume, device)?;
        let path = self.path_of(volume);
        let flags = MountFlags::NOSUID | MountFlags::NODEV;
        mount(device, &path, FILESYSTEM, flags, None::<&std::ffi::CStr>)
            .map_err(|e| HelperError::Failed(format!("mounting volume {volume}: {e}")))?;
        // The filesystem's root, as the workload's root may write it and anyone in it read.
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        Ok(created)
    }

    /// Unmounts the volume `volume` and lets go of its device, which the storage keeps; whether a
    /// device was attached for it.
    pub(crate) fn release_device(&self, volume: &str) -> HelperResult<bool> {
        identifier("volume", volume)?;
        let path = self.path_of(volume);
        if self.mounted(&path)? {
            rustix::mount::unmount(&path, rustix::mount::UnmountFlags::empty())
                .map_err(|e| HelperError::Failed(format!("unmounting volume {volume}: {e}")))?;
        }
        // A guest's disk leaves its device node in the directory, never a file anyone wrote.
        if crate::block::is_block(&path) {
            let node = path.join(crate::block::IMAGE);
            match std::fs::symlink_metadata(&node) {
                Ok(m) if std::os::unix::fs::FileTypeExt::is_block_device(&m.file_type()) => {
                    std::fs::remove_file(&node)?;
                }
                _ => {}
            }
        }
        match rustix::fs::unlinkat(&self.handle, volume, rustix::fs::AtFlags::REMOVEDIR) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => {}
            Err(e) => return Err(std::io::Error::from(e).into()),
        }
        let Some(devices) = self.devices.as_ref() else { return Ok(false) };
        let released = devices.release(volume)?;
        if released {
            tracing::info!(%volume, "device released");
        }
        Ok(released)
    }

    /// What the volume `volume`'s filesystem accounts, its bound the device's size.
    pub(crate) fn device_usage(&self, volume: &str) -> HelperResult<helper::VolumeUsageResponse> {
        identifier("volume", volume)?;
        let path = self.path_of(volume);
        if crate::block::is_block(&path) {
            // A guest's disk: what it holds only the guest's kernel knows.
            return Ok(helper::VolumeUsageResponse { allocated: true, ..Default::default() });
        }
        if !self.mounted(&path)? {
            return Err(HelperError::NotFound(format!("volume {volume} is not attached here")));
        }
        let s = rustix::fs::statvfs(&path).map_err(std::io::Error::from)?;
        let used = s.f_blocks.saturating_sub(s.f_bfree).saturating_mul(s.f_frsize);
        Ok(helper::VolumeUsageResponse {
            used_bytes: used,
            used_inodes: s.f_files.saturating_sub(s.f_ffree),
            limit_bytes: s.f_blocks.saturating_mul(s.f_frsize),
            limit_inodes: s.f_files,
            accounted: true,
            ..Default::default()
        })
    }
}
