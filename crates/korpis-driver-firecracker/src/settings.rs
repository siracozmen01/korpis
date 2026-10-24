// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where the driver finds what it runs, and where it keeps what it makes: one directory of its
//! own, holding each object's record and config drive, the drives its guests share, and the
//! jailer's chroots, all on one filesystem so a drive reaches a chroot as a hard link.

use std::path::{Path, PathBuf};

/// The unified cgroup hierarchy.
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// The driver's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Firecracker, which the jailer copies into each guest's chroot.
    pub firecracker: PathBuf,
    /// Firecracker's jailer.
    pub jailer: PathBuf,
    /// The guest kernel every guest boots (§6 of docs/04-runtimes.md).
    pub kernel: PathBuf,
    /// `korpis-guest`, static, for the boot drive.
    pub guest: PathBuf,
    /// `korpis-fsworker`, built static, which the boot drive carries for file sessions in a guest
    /// (§4.4 of docs/04-runtimes.md).
    pub fsworker: PathBuf,
    /// `korpis-install-job`, built static, which the boot drive carries for an install in a
    /// service guest (§4.4 of docs/04-runtimes.md).
    pub install_job: PathBuf,
    /// `korpis-image-build`, which makes an image's drive.
    pub image_build: PathBuf,
    /// `korpis-stream-writer`, which writes a guest's console to its stream.
    pub writer: PathBuf,
    /// The node's content store, which the agent fills and the driver only reads.
    pub content: PathBuf,
    /// The directory the helper keeps volumes under; a volume's drive is nowhere else.
    pub volume_root: PathBuf,
    /// The directory the helper binds workloads' network namespaces under, one each (§4.1 of
    /// docs/07-networking.md): the only namespaces a guest's Firecracker joins.
    pub netns_root: PathBuf,
    /// The directory the agent makes workloads' identity directories under (§9.5 of
    /// docs/08-identity.md), whose sockets a guest's are relayed to.
    pub identity_root: PathBuf,
    /// The driver's own directory.
    pub state: PathBuf,
    /// The cgroup, from the unified hierarchy's root, beneath which each guest's is made.
    pub cgroup: String,
}

impl Settings {
    /// Each object's directory is here, named by its identifier.
    #[must_use]
    pub fn objects(&self) -> PathBuf {
        self.state.join("objects")
    }

    /// The object `id`'s directory.
    #[must_use]
    pub fn object(&self, id: &str) -> PathBuf {
        self.objects().join(id)
    }

    /// The drives guests share: boot drives, image drives, and the kernel.
    #[must_use]
    pub fn drives(&self) -> PathBuf {
        self.state.join("drives")
    }

    /// The service guests' directories (§4.4 of docs/04-runtimes.md), each named by its jail.
    #[must_use]
    pub fn services(&self) -> PathBuf {
        self.state.join("services")
    }

    /// Where the jailer builds its chroots.
    #[must_use]
    pub fn jails(&self) -> PathBuf {
        self.state.join("jail")
    }

    /// The jailer's directory of every jail it builds, named by Firecracker's file name.
    #[must_use]
    pub fn jails_of_firecracker(&self) -> PathBuf {
        self.jails().join(self.firecracker.file_name().unwrap_or_default())
    }

    /// The jail of object `id`, which the jailer names by Firecracker's file name and the id.
    #[must_use]
    pub fn jail(&self, id: &str) -> PathBuf {
        self.jails_of_firecracker().join(id)
    }

    /// The root the guest's Firecracker sees.
    #[must_use]
    pub fn chroot(&self, id: &str) -> PathBuf {
        self.jail(id).join("root")
    }

    /// The cgroup of object `id`'s Firecracker, as the jailer makes it.
    #[must_use]
    pub fn cgroup_of(&self, id: &str) -> PathBuf {
        Path::new(CGROUP_ROOT).join(&self.cgroup).join(id)
    }

    /// The cgroup beneath which every guest's is.
    #[must_use]
    pub fn cgroup_parent(&self) -> PathBuf {
        Path::new(CGROUP_ROOT).join(&self.cgroup)
    }
}
