// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Checkpoint`, and the start that resumes one. A checkpoint pauses the guest, has its Firecracker
//! write the machine's state and memory into its chroot, copies them and the drives the guest
//! holds into the directory asked for ([`crate::state`]), and ends the Firecracker: the object is
//! then stopped. A resume copies them back into the object's directory, links the state and memory
//! into its chroot as its Firecracker's to read, starts a Firecracker configured with nothing, and
//! has it load them and resume the guest, which goes on from where it was paused, holding the bytes
//! it booted from.
//!
//! A state is resumed only where it restores: by the same Firecracker version, with the same CPU
//! template, on a host kernel of the same `major.minor`, as what took it recorded. Any other is
//! refused, never booted in its place, since a workload that was running is not silently started
//! afresh.

use super::{Machine, MachineError, MachineResult, failed};
use crate::jail::{self, Drives};
use crate::settings::Settings;
use crate::state::{self, DRIVES, MEMORY, Taken, VMSTATE};
use crate::{api, process};
use rustix::fs::{Mode, OFlags};
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

/// The directory beside an object's record holding what it resumes from.
const RESUMED: &str = "resumed";

fn refused(why: impl Into<String>) -> MachineError {
    MachineError::Refused(why.into())
}

fn opened(dir: &Path) -> io::Result<OwnedFd> {
    let flags = OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(rustix::fs::open(dir, flags, Mode::empty())?)
}

/// What this node's Firecracker is, as a state taken here records it: none where its version
/// cannot be read, so nothing is checkpointed or resumed.
#[must_use]
pub fn here(firecracker: &Path) -> Option<Taken> {
    let out = std::process::Command::new(firecracker).arg("--version").output().ok()?;
    let said = String::from_utf8_lossy(&out.stdout);
    let version = said.lines().next()?.trim().strip_prefix("Firecracker v")?;
    out.status.success().then(|| Taken {
        hypervisor_version: version.to_owned(),
        cpu_template: String::new(),
        kernel: state::kernel(),
    })
}

/// The roots a state's directory may be directly beneath: the node's volume roots.
fn roots(settings: &Settings) -> Vec<&Path> {
    std::iter::once(settings.volume_root.as_path())
        .chain(settings.shared_roots.iter().map(PathBuf::as_path))
        .collect()
}

impl Machine {
    /// Pauses object `id`'s running guest, writes its state into the directory `into`, and ends
    /// it; the bytes written. A guest whose state could not be written is resumed as it was.
    pub async fn checkpoint(&self, id: &str, into: &Path) -> MachineResult<u64> {
        let taken = self.here.clone().ok_or_else(|| refused("no snapshot is taken here"))?;
        let to = state::dir(&roots(&self.settings), into).map_err(|e| refused(e.to_string()))?;
        let _held = self.lock.lock().await;
        let object = self.load(id)?;
        let vm = object.vm.filter(process::alive);
        let vm = vm.ok_or_else(|| refused(format!("{id} does not run")))?;
        let socket = jail::api_socket(&self.settings, id);
        let chroot = self.settings.chroot(id);
        let written = async {
            api::snapshot(&socket, &format!("/{VMSTATE}"), &format!("/{MEMORY}")).await?;
            let from = opened(&chroot)?;
            let kept = tokio::task::spawn_blocking(move || state::keep(&from, &to, &taken));
            kept.await.map_err(io::Error::other)?
        }
        .await;
        for name in [VMSTATE, MEMORY] {
            let _ = std::fs::remove_file(chroot.join(name));
        }
        let bytes = match written {
            Ok(bytes) => bytes,
            Err(e) => {
                // Paused or not, it is let go on; what failed is said.
                let _ = api::resume(&socket).await;
                return Err(failed(format!("the checkpoint: {e}")));
            }
        };
        if let Some(fd) = process::descriptor(&vm) {
            process::kill(&vm);
            process::ended(fd).await;
        }
        Ok(bytes)
    }

    /// Starts object `id`'s guest from the state in the directory `from`, which a checkpoint
    /// wrote where it restores here; one already running is left so.
    pub async fn resume(&self, id: &str, from: &Path) -> MachineResult<()> {
        let here = self.here.as_ref().ok_or_else(|| refused("no snapshot restores here"))?;
        let dir = state::dir(&roots(&self.settings), from).map_err(|e| refused(e.to_string()))?;
        let taken = state::taken(&dir).map_err(|e| refused(format!("no state: {e}")))?;
        if taken != *here {
            return Err(refused(format!(
                "a state taken by {taken:?} does not restore on {here:?}"
            )));
        }
        self.run(id, Some(dir)).await
    }
}

/// The drives object `dir`, owned by `uid`, resumes the state in `from` holding: the state's
/// copied into the object's directory, its config drive replacing the object's.
pub(super) fn resumed(
    settings: &Settings,
    dir: &Path,
    from: &OwnedFd,
    uid: u32,
    kept: Drives,
) -> io::Result<Drives> {
    let at = dir.join(RESUMED);
    std::fs::create_dir_all(&at)?;
    state::take(from, &opened(&at)?)?;
    let config = crate::object::Object::config(dir);
    std::fs::rename(at.join("config"), &config)?;
    std::os::unix::fs::chown(&config, Some(uid), Some(uid))?;
    let current = super::create::current(settings, kept)?;
    Ok(Drives { boot: at.join(DRIVES[0]), image: at.join(DRIVES[1]), config, ..current })
}

/// Links the state object `id`, in `dir`, resumes into its chroot, its Firecracker's, `uid`'s, to
/// read alone.
pub(super) fn place(settings: &Settings, id: &str, dir: &Path, uid: u32) -> io::Result<()> {
    let chroot = settings.chroot(id);
    for name in [VMSTATE, MEMORY] {
        let from = dir.join(RESUMED).join(name);
        std::os::unix::fs::chown(&from, Some(uid), Some(uid))?;
        std::fs::hard_link(&from, chroot.join(name))?;
    }
    Ok(())
}

/// Has object `id`'s Firecracker, in `dir`, load the state placed in its chroot and resume the
/// guest; then the copies are let go, the memory staying mapped by that Firecracker alone.
pub(super) async fn load(settings: &Settings, id: &str, dir: &Path) -> MachineResult<()> {
    let socket = jail::api_socket(settings, id);
    let until = std::time::Instant::now() + super::JAILER_BOUND;
    while api::socket_at(&socket).is_err() && std::time::Instant::now() < until {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let loaded = api::load(&socket, &format!("/{VMSTATE}"), &format!("/{MEMORY}")).await;
    let chroot = settings.chroot(id);
    for name in [VMSTATE, MEMORY] {
        let _ = std::fs::remove_file(chroot.join(name));
        let _ = std::fs::remove_file(dir.join(RESUMED).join(name));
    }
    loaded.map_err(|e| failed(format!("the state did not load: {e}")))
}
