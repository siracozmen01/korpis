// SPDX-License-Identifier: AGPL-3.0-or-later

//! Block storage providers' calls (§2.1 of docs/06-storage.md, §5 of docs/16-extensions.md): one
//! Korpis volume is one device of the storage, which the provider names; it is made, attached to
//! one node for one lease epoch after every other node is refused it, detached, deleted, and
//! described. Each call says the whole of what is wanted, so a repeat changes nothing.

use uuid::Uuid;

/// The longest name a provider may give a device.
pub const MAX_DEVICE: usize = 256;
/// The longest attachment target.
pub const MAX_TARGET: usize = 1024;
/// The longest attachment credential.
pub const MAX_CREDENTIAL: usize = 4096;

/// How a node attaches a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BlockProtocol {
    /// An iSCSI LUN, logged in with open-iscsi; the target is `<portal>|<iqn>|<lun>`.
    Iscsi,
    /// A Ceph RBD image, mapped through the kernel's RBD interface; the target is
    /// `<monitors>|<pool>|<image>`.
    Rbd,
    /// A file in a directory the node's operator named, attached through a loop device; the
    /// target is the file's name there.
    Loop,
}

impl BlockProtocol {
    /// Every protocol.
    pub const ALL: [Self; 3] = [Self::Iscsi, Self::Rbd, Self::Loop];

    /// The name a node's configuration writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Iscsi => "iscsi",
            Self::Rbd => "rbd",
            Self::Loop => "loop",
        }
    }

    /// The protocol a name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// Whether `target` is one this protocol can attach: three fields for iSCSI and RBD, and for
    /// a loop device a file name alone, so no target names a file outside the directory the
    /// node's operator named.
    #[must_use]
    pub fn takes(self, target: &str) -> bool {
        let plain = !target.is_empty()
            && target.len() <= MAX_TARGET
            && !target.chars().any(char::is_control);
        plain
            && match self {
                Self::Iscsi | Self::Rbd => {
                    let fields: Vec<&str> = target.split('|').collect();
                    fields.len() == 3 && fields.iter().all(|f| !f.is_empty())
                }
                Self::Loop => {
                    target.len() <= 255 && !target.contains('/') && target != "." && target != ".."
                }
            }
    }
}

/// A device asked for one volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockCreate {
    /// The volume: a provider asked again for one it made answers the device it made.
    pub volume: Uuid,
    /// The class it is in.
    pub storage_class: String,
    /// Its size.
    pub size_bytes: u64,
}

/// A device attached to one node for one lease epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAttach {
    /// The provider's name for it.
    pub device: String,
    /// The node to attach it to.
    pub node: Uuid,
    /// The node's lease epoch for the workload holding it; a lower one than the provider has
    /// attached it for is refused.
    pub epoch: u64,
    /// What the node's helper can attach with.
    pub protocols: Vec<BlockProtocol>,
}

/// What a node needs to attach a device. The credential is the device's alone where the
/// storage can scope one, and is never written to a log.
#[derive(Clone, PartialEq, Eq)]
pub struct Attachment {
    /// How to attach it.
    pub protocol: BlockProtocol,
    /// Where it is, as the protocol names it.
    pub target: String,
    /// What the node presents for it; empty where none is needed.
    pub credential: String,
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("protocol", &self.protocol)
            .field("target", &self.target)
            .field("credential", &if self.credential.is_empty() { "" } else { "<redacted>" })
            .finish()
    }
}

impl Attachment {
    /// Whether it answers `asked`: a protocol the node speaks, a target that protocol takes,
    /// and a credential within bounds.
    #[must_use]
    pub fn answers(&self, asked: &BlockAttach) -> bool {
        asked.protocols.contains(&self.protocol)
            && self.protocol.takes(&self.target)
            && self.credential.len() <= MAX_CREDENTIAL
            && !self.credential.chars().any(char::is_control)
    }
}

/// What the storage says of a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDescription {
    /// Its size.
    pub size_bytes: u64,
    /// The node it is attached to, and for which epoch; `None` while attached to none.
    pub attached: Option<(Uuid, u64)>,
}

/// Whether `device` is a name a provider may give a device.
#[must_use]
pub fn is_device(device: &str) -> bool {
    !device.is_empty() && device.len() <= MAX_DEVICE && !device.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_one_its_protocol_takes_and_a_loop_never_leaves_its_directory() {
        assert!(BlockProtocol::Iscsi.takes("10.0.0.5:3260|iqn.2026-10.org.example:vol|1"));
        assert!(!BlockProtocol::Iscsi.takes("10.0.0.5:3260|iqn"), "three fields");
        assert!(BlockProtocol::Rbd.takes("10.0.0.1,10.0.0.2|rbd|vol_1"));
        assert!(BlockProtocol::Loop.takes("vol_1.img"));
        for escape in ["../etc/shadow", "/dev/sda", "..", ".", ""] {
            assert!(!BlockProtocol::Loop.takes(escape), "{escape}");
        }
        let asked = BlockAttach {
            device: "d".into(),
            node: Uuid::nil(),
            epoch: 3,
            protocols: vec![BlockProtocol::Loop],
        };
        let a = Attachment {
            protocol: BlockProtocol::Loop,
            target: "d.img".into(),
            credential: "secret".into(),
        };
        assert!(a.answers(&asked));
        assert!(!format!("{a:?}").contains("secret"), "never logged");
        let iscsi = Attachment { protocol: BlockProtocol::Iscsi, ..a };
        assert!(!iscsi.answers(&asked), "a protocol the node does not speak");
    }
}
