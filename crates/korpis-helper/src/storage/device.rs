// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class's devices on this node (§2.1 of docs/06-storage.md): each attached by the
//! kernel's own initiator, as the storage's provider answered for this node and this lease's
//! epoch, and let go of again. The provider's answer comes from an extension, so every part of it
//! that reaches a command line, a sysfs write, or a path is checked against what the protocol
//! allows before it is used: a loop target is a bare file name in the directory the operator
//! named, an iSCSI target a portal, a qualified name and a number, an RBD target monitors, a pool
//! and an image, each from a closed set of characters. What was attached for a volume, and for
//! which workload, is recorded beside the class, the credential never, so that letting go of it
//! needs nothing but the volume, and the agent can tell, after either restarts, what is attached.

use crate::error::{HelperError, HelperResult};
use std::path::{Path, PathBuf};

mod iscsi;
mod loopdev;
mod rbd;

/// How the class's devices are attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protocol {
    /// An iSCSI LUN, logged in with open-iscsi.
    Iscsi,
    /// A Ceph RBD image, mapped through the kernel's RBD interface.
    Rbd,
    /// A file in this directory, through a loop device.
    Loop(PathBuf),
}

impl Protocol {
    /// The protocol `--block-class NAME=` names: `iscsi`, `rbd`, or `loop:DIR`.
    ///
    /// # Errors
    /// Anything else, or a loop directory that is not absolute.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.split_once(':') {
            None if s == "iscsi" => Ok(Self::Iscsi),
            None if s == "rbd" => Ok(Self::Rbd),
            Some(("loop", dir)) if Path::new(dir).is_absolute() => Ok(Self::Loop(dir.into())),
            _ => Err(format!("{s:?}: iscsi, rbd, or loop:/an/absolute/directory")),
        }
    }

    /// Its wire form.
    #[must_use]
    pub fn to_pb(&self) -> korpis_proto::korpis::v1::BlockProtocol {
        use korpis_proto::korpis::v1::BlockProtocol as P;
        match self {
            Self::Iscsi => P::BLOCK_PROTOCOL_ISCSI,
            Self::Rbd => P::BLOCK_PROTOCOL_RBD,
            Self::Loop(_) => P::BLOCK_PROTOCOL_LOOP,
        }
    }
}

/// A block class's devices: how they are attached, and where what was attached is recorded.
#[derive(Debug)]
pub struct Devices {
    protocol: Protocol,
    records: PathBuf,
}

/// The characters a part of a target may hold: none a command line, a sysfs write, or a path
/// could take for anything but itself.
pub(super) fn plain(what: &str, s: &str, extra: &str) -> HelperResult<()> {
    let ok = !s.is_empty()
        && s.len() <= 255
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c) || extra.contains(c));
    if ok { Ok(()) } else { Err(HelperError::Refused(format!("{what} {s:?} is not one"))) }
}

/// What `protocol` needs of the node that is missing, if anything: the helper loads no module and
/// installs nothing, so a class whose protocol the node lacks is refused, saying which.
fn missing(protocol: &Protocol) -> Option<String> {
    let runs = |tool: &str, flag: &str| {
        std::process::Command::new(tool).arg(flag).output().is_ok_and(|o| o.status.success())
    };
    let needs: Vec<(bool, &str)> = match protocol {
        Protocol::Loop(dir) => vec![
            (std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()), "the loop directory"),
            (Path::new("/dev/loop-control").exists(), "the loop driver"),
        ],
        Protocol::Iscsi => vec![
            (Path::new("/sys/module/iscsi_tcp").exists(), "the iscsi_tcp module"),
            (runs("iscsiadm", "--version"), "open-iscsi's iscsiadm"),
        ],
        Protocol::Rbd => vec![(Path::new(RBD_ADD).exists(), "the kernel's rbd module")],
    };
    let tools = [(runs("mke2fs", "-V"), "e2fsprogs' mke2fs"), (runs("blkid", "-V"), "blkid")];
    let lacking: Vec<&str> =
        needs.into_iter().chain(tools).filter(|(found, _)| !found).map(|(_, n)| n).collect();
    (!lacking.is_empty()).then(|| lacking.join(", "))
}

/// Where the kernel's RBD interface maps an image, present when its module is loaded.
const RBD_ADD: &str = "/sys/bus/rbd/add_single_major";

impl Devices {
    /// The devices attached by `protocol`, recorded under `records`, made if missing; refused
    /// when the node lacks what the protocol needs.
    pub fn open(protocol: Protocol, records: &Path) -> std::io::Result<Self> {
        if let Some(lacking) = missing(&protocol) {
            let why = format!("the node lacks {lacking}, and the helper loads nothing");
            return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, why));
        }
        std::fs::create_dir_all(records)?;
        Ok(Self { protocol, records: records.to_owned() })
    }

    /// How they are attached.
    #[must_use]
    pub fn protocol(&self) -> &Protocol {
        &self.protocol
    }

    /// Attaches the device `target` names for the volume `volume` of `workload`, presenting
    /// `credential`; the device's path. Attaching what is attached already finds it.
    pub fn attach(
        &self,
        (volume, workload): (&str, &str),
        target: &str,
        credential: &str,
    ) -> HelperResult<PathBuf> {
        let device = match &self.protocol {
            Protocol::Loop(dir) => loopdev::attach(dir, target)?,
            Protocol::Iscsi => iscsi::attach(&iscsi::Target::parse(target)?, credential)?,
            Protocol::Rbd => rbd::attach(&rbd::Target::parse(target)?, credential)?,
        };
        std::fs::write(self.records.join(volume), format!("{workload}\n{target}"))?;
        Ok(device)
    }

    /// Each volume a device is attached for, with its workload.
    pub fn attached(&self) -> HelperResult<Vec<(String, String)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.records)? {
            let entry = entry?;
            let Some(volume) = entry.file_name().to_str().map(str::to_owned) else { continue };
            let record = std::fs::read_to_string(entry.path())?;
            let workload = record.split_once('\n').map_or("", |(w, _)| w).to_owned();
            out.push((volume, workload));
        }
        Ok(out)
    }

    /// Lets go of the device attached for `volume`, if one is; whether one was. Its filesystem
    /// is unmounted by then.
    pub fn release(&self, volume: &str) -> HelperResult<bool> {
        let record = self.records.join(volume);
        let target = match std::fs::read_to_string(&record) {
            Ok(r) => r.split_once('\n').map_or(r.clone(), |(_, t)| t.to_owned()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        match &self.protocol {
            Protocol::Loop(dir) => loopdev::release(dir, &target)?,
            Protocol::Iscsi => iscsi::release(&iscsi::Target::parse(&target)?)?,
            Protocol::Rbd => rbd::release(&rbd::Target::parse(&target)?)?,
        }
        std::fs::remove_file(record)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_names_its_protocol_and_a_loop_class_its_directory() {
        assert_eq!(Protocol::parse("iscsi"), Ok(Protocol::Iscsi));
        assert_eq!(Protocol::parse("loop:/srv/disks"), Ok(Protocol::Loop("/srv/disks".into())));
        assert!(Protocol::parse("loop:disks").is_err(), "relative");
        assert!(Protocol::parse("nbd").is_err());
        assert!(plain("name", "a.b-c_1", "").is_ok());
        for bad in ["", "a b", "a/b", "a\nb", "a,b"] {
            assert!(plain("name", bad, "").is_err(), "{bad:?}");
        }
    }
}
