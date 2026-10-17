// SPDX-License-Identifier: AGPL-3.0-or-later

//! What preflight asks of things other than files: the kernel's Landlock, `nft`, containerd's
//! socket, the helper, and the block device the volumes are on. Each is only asked: `nft` lists
//! the ruleset and changes nothing, and Landlock rulesets are made and never applied.

use super::{Offer, Probes};
use crate::host::privileged::HelperClient;
use std::path::{Path, PathBuf};

/// Where preflight asks.
#[derive(Debug, Clone)]
pub struct Asking {
    /// What the node offers.
    pub offer: Offer,
    /// containerd's socket, for the oci driver.
    pub containerd: PathBuf,
    /// The privileged helper's socket.
    pub helper: PathBuf,
    /// The helper's volume root.
    pub volume_root: PathBuf,
    /// `/sys`, for the block devices.
    pub sys: PathBuf,
}

/// Asks everything.
pub async fn probe(a: &Asking) -> Probes {
    let containerd = (a.offer.driver == "oci").then(|| {
        std::os::unix::net::UnixStream::connect(&a.containerd)
            .map(drop)
            .map_err(|e| format!("{}: {e}", a.containerd.display()))
    });
    let storage = HelperClient::new(&a.helper).describe_storage().await.map_err(|e| e.to_string());
    Probes {
        landlock_abi: landlock_abi(),
        nft: nft().await,
        containerd,
        storage: Some(storage),
        volume_disk: volume_disk(&a.volume_root, &a.sys),
    }
}

/// The newest Landlock ABI whose file access rights the kernel handles. Only ABIs 1, 2, 3, and
/// 5 changed those rights, so a kernel speaking 4 is told as 3 and one speaking 6 or later as 5;
/// the file worker asks for the rights of 5 and takes what the kernel has.
fn landlock_abi() -> Option<u8> {
    use landlock::RulesetAttr as _;
    use landlock::{ABI, Access as _, AccessFs, CompatLevel, Compatible as _, Ruleset};
    [(ABI::V5, 5), (ABI::V3, 3), (ABI::V2, 2), (ABI::V1, 1)].into_iter().find_map(|(abi, n)| {
        Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(abi))
            .and_then(Ruleset::create)
            .ok()
            .map(|_| n)
    })
}

/// What `nft --version` prints, and whether `nft -j list tables` answers.
async fn nft() -> Option<(String, bool)> {
    let version = tokio::process::Command::new("nft").arg("--version").output().await.ok()?;
    if !version.status.success() {
        return None;
    }
    let printed = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    let listed = tokio::process::Command::new("nft").args(["-j", "list", "tables"]).output().await;
    let json = listed.is_ok_and(|o| o.status.success() && o.stdout.starts_with(b"{"));
    Some((printed, json))
}

/// The device `root` is on, and the disk it is part of, as `major:minor`; the disk is where
/// io.cost is configured, so a partition is followed to the disk that holds it.
fn volume_disk(root: &Path, sys: &Path) -> Option<(String, String)> {
    let st = rustix::fs::stat(root).ok()?;
    let dev = format!("{}:{}", rustix::fs::major(st.st_dev), rustix::fs::minor(st.st_dev));
    let block = sys.join("dev/block").join(&dev);
    let disk = if block.join("partition").exists() {
        let parent = std::fs::canonicalize(&block).ok()?.parent()?.join("dev");
        std::fs::read_to_string(parent).ok()?.trim().to_owned()
    } else {
        dev.clone()
    };
    Some((dev, disk))
}
