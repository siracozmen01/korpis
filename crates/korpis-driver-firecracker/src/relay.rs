// SPDX-License-Identifier: AGPL-3.0-or-later

//! The host's half of a guest's `/run/korpis` (§9.5 of docs/08-identity.md, §6 of
//! docs/04-runtimes.md). A connection the guest opens to its host on the identity port or the
//! API port arrives, through its Firecracker, on a socket the driver listens on in the guest's
//! chroot (`vsock.socket_<port>`), and is relayed, byte for byte, to the workload's own socket of
//! that name in the directory the agent serves it: the driver reads nothing that crosses, and the
//! host's socket is the workload's identity, as for a container. The workload's socket is opened as
//! the API socket is, never through a link. A task beside them tells the guest's agent whether
//! the API relay is served, which the agent's directory says, as it changes. At most sixteen
//! connections are relayed at once for each guest.

use crate::agent;
use crate::api::socket_at;
use korpis_guest::control;
use std::collections::HashMap;
use std::io;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

/// The workload's identity socket in its directory on the host.
const IDENTITY: &str = "identity.sock";
/// The API relay beside it, where its node relays the API to it.
const API: &str = "api.sock";
/// The most connections relayed at once for one guest.
const MOST_RELAYED: usize = 16;
/// How often the agent's directory is looked at for the API relay.
const LOOK: Duration = Duration::from_secs(2);
/// How long a guest's agent is given to take what it is told.
const TELL: Duration = Duration::from_secs(5);

/// The relays of the guests that run, by object.
#[derive(Debug, Default)]
pub struct Relays(Mutex<HashMap<String, Vec<JoinHandle<()>>>>);

/// Where one guest's relays are: its chroot, its vsock device's socket there, the workload's
/// directory on the host, and whose they are.
#[derive(Debug, Clone)]
pub struct Guest {
    /// The guest's chroot, where its Firecracker connects for each port.
    pub chroot: PathBuf,
    /// Its vsock device's socket, through which its agent is told.
    pub vsock: PathBuf,
    /// The workload's directory on the host.
    pub identity: PathBuf,
    /// The user and group its Firecracker runs as, the only ones that may connect to its relays:
    /// what reaches them reaches the workload's identity.
    pub owner: (u32, u32),
    /// A directory only the driver reaches, on the chroot's filesystem: the object's own.
    pub private: PathBuf,
}

impl Relays {
    /// Serves object `id`'s guest its relays, replacing any it had.
    ///
    /// # Errors
    /// A socket in its chroot cannot be bound.
    pub fn serve(&self, id: &str, guest: &Guest) -> io::Result<()> {
        self.end(id);
        let limit = Arc::new(Semaphore::new(MOST_RELAYED));
        let mut tasks = Vec::new();
        for (port, name) in [(control::IDENTITY_PORT, IDENTITY), (control::API_PORT, API)] {
            let listener = bind(guest, port)?;
            let to = guest.identity.join(name);
            tasks.push(tokio::spawn(accepting(listener, to, Arc::clone(&limit))));
        }
        tasks.push(tokio::spawn(telling(guest.vsock.clone(), guest.identity.join(API))));
        self.lock().insert(id.to_owned(), tasks);
        Ok(())
    }

    /// Ends object `id`'s relays, if it has any.
    pub fn end(&self, id: &str) {
        for task in self.lock().remove(id).unwrap_or_default() {
            task.abort();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<JoinHandle<()>>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A listener for `port` in `guest`'s chroot (`vsock.socket_<port>`), which only its owner, the
/// guest's Firecracker, may connect to. Firecracker can write the chroot, so nothing is done there
/// by path but the last step: the socket is bound in the object's own directory, which only the
/// driver reaches, given its owner and mode there, and renamed into the chroot, which replaces
/// whatever is in its place, a link included, without following it.
fn bind(guest: &Guest, port: u32) -> io::Result<UnixListener> {
    let name = format!("vsock.socket_{port}");
    let made = guest.private.join(&name);
    match std::fs::remove_file(&made) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = std::os::unix::net::UnixListener::bind(&made)?;
    std::os::unix::fs::lchown(&made, Some(guest.owner.0), Some(guest.owner.1))?;
    std::fs::set_permissions(&made, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&made, guest.chroot.join(&name))?;
    listener.set_nonblocking(true)?;
    UnixListener::from_std(listener)
}

/// Relays each connection on `listener` to the socket at `to`, at most `limit` at once.
async fn accepting(listener: UnixListener, to: PathBuf, limit: Arc<Semaphore>) {
    while let Ok((guest, _)) = listener.accept().await {
        let Ok(held) = Arc::clone(&limit).try_acquire_owned() else { continue };
        let to = to.clone();
        tokio::spawn(async move {
            if let Err(e) = relayed(guest, &to).await {
                tracing::debug!(error = %e, to = %to.display(), "a guest's relay ended");
            }
            drop(held);
        });
    }
}

/// Copies `guest` and the workload's socket at `to` into each other until either ends.
async fn relayed(mut guest: UnixStream, to: &Path) -> io::Result<()> {
    let fd = socket_at(to)?;
    let mut host = UnixStream::connect(format!("/proc/self/fd/{}", fd.as_raw_fd())).await?;
    drop(fd);
    tokio::io::copy_bidirectional(&mut guest, &mut host).await?;
    Ok(())
}

/// Tells the guest's agent, through `vsock`, whether the API relay at `api` is served, whenever
/// that changes from what the agent last said it serves; an agent that has not yet entered the
/// workload's root, or did not answer, is told again.
async fn telling(vsock: PathBuf, api: PathBuf) {
    let mut serves = None;
    loop {
        let wanted = control::Relays { api: api.exists() };
        if serves != Some(wanted) {
            serves = agent::relays(&vsock, wanted, TELL).await.ok();
        }
        tokio::time::sleep(LOOK).await;
    }
}
