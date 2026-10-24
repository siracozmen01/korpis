// SPDX-License-Identifier: AGPL-3.0-or-later

//! The workload's `/run/korpis` inside its guest (§9.5 of docs/08-identity.md, §6 of
//! docs/04-runtimes.md): its identity socket, and the API relay beside it where its node relays
//! the API to it, each a Unix socket the first process listens on in a directory only root
//! writes. A connection on either is relayed, byte for byte, over the guest's vsock device to the
//! host's port for it, where the driver relays it on to the workload's own socket on the host:
//! the guest reads nothing of what crosses, and the host's socket is the workload's identity, as
//! it is a container's. At most sixteen connections are relayed at once.

use korpis_guest::control;
use rustix::net::{AddressFamily, SocketType};
use std::io;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// The directory, inside the workload's root.
pub const DIR: &str = "/run/korpis";
/// The identity socket's name in it.
const IDENTITY: &str = "identity.sock";
/// The API relay's name in it.
const API: &str = "api.sock";
/// The most connections relayed at once.
const MOST_RELAYED: usize = 16;

/// The API relay while it is served: whether it should be, and the flag that stops its listener.
static API_RELAY: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);
/// Whether the identity socket is served, in the workload's root, beside which the API relay
/// may be: until the first process has entered that root, a socket would be made outside it.
static SERVED: AtomicBool = AtomicBool::new(false);

/// Makes the directory and serves the identity socket in it, in a thread of its own.
///
/// # Errors
/// The directory or the socket cannot be made.
pub fn serve() -> Result<(), String> {
    let dir = Path::new(DIR);
    std::fs::create_dir_all(dir).map_err(|e| format!("{DIR}: {e}"))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("{DIR}: {e}"))?;
    let listener = bound(&dir.join(IDENTITY))?;
    let open = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || accepting(&listener, control::IDENTITY_PORT, &open, &stop));
    SERVED.store(true, Ordering::Release);
    Ok(())
}

/// Whether the identity socket is served in the workload's root, so `/run/korpis` there is the
/// workload's and what is written into it lands inside its root.
pub fn served() -> bool {
    SERVED.load(Ordering::Acquire)
}

/// Serves the relays `wanted`, as the host says, and stops serving the others; what is served
/// after, which is less than asked if the API relay's socket could not be made.
pub fn relays(wanted: control::Relays) -> control::Relays {
    let served = api(wanted.api).is_ok_and(|()| wanted.api);
    control::Relays { api: served }
}

/// Serves the API relay if `wanted`, and stops serving it otherwise.
fn api(wanted: bool) -> Result<(), String> {
    if !SERVED.load(Ordering::Acquire) {
        return Err("the workload's identity socket is not served yet".into());
    }
    let mut serving = API_RELAY.lock().unwrap_or_else(PoisonError::into_inner);
    match (serving.as_ref(), wanted) {
        (Some(_), true) | (None, false) => Ok(()),
        (Some(stop), false) => {
            // Its listener sees the flag at its next connection, which this is.
            stop.store(true, Ordering::Release);
            let path = Path::new(DIR).join(API);
            let _ = UnixStream::connect(&path);
            let _ = std::fs::remove_file(path);
            *serving = None;
            Ok(())
        }
        (None, true) => {
            let listener = bound(&Path::new(DIR).join(API))?;
            let stop = Arc::new(AtomicBool::new(false));
            let (open, stopping) = (Arc::new(AtomicUsize::new(0)), Arc::clone(&stop));
            std::thread::spawn(move || accepting(&listener, control::API_PORT, &open, &stopping));
            *serving = Some(stop);
            Ok(())
        }
    }
}

/// A listener at `path`, which any user of the guest may connect to: the directory is what
/// confines it, as the host's identity root does.
fn bound(path: &Path) -> Result<UnixListener, String> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(listener)
}

/// Relays each connection on `listener` to the host's `port` until `stop` is set.
fn accepting(listener: &UnixListener, port: u32, open: &Arc<AtomicUsize>, stop: &AtomicBool) {
    for client in listener.incoming() {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(client) = client else { continue };
        if open.load(Ordering::Acquire) >= MOST_RELAYED {
            continue;
        }
        open.fetch_add(1, Ordering::AcqRel);
        let open = Arc::clone(open);
        std::thread::spawn(move || {
            let _ = relayed(client, port);
            open.fetch_sub(1, Ordering::AcqRel);
        });
    }
}

/// A connection to the host's `port` over the guest's vsock device.
fn host(port: u32) -> io::Result<OwnedFd> {
    let fd = rustix::net::socket(AddressFamily::VSOCK, SocketType::STREAM, None)?;
    let at = nix::sys::socket::VsockAddr::new(control::HOST_CID, port);
    nix::sys::socket::connect(fd.as_raw_fd(), &at).map_err(io::Error::from)?;
    Ok(fd)
}

/// Copies `client` and the host's `port` into each other until either side ends.
fn relayed(client: UnixStream, port: u32) -> io::Result<()> {
    let upstream = std::fs::File::from(host(port)?);
    let (mut from_client, mut to_host) = (client.try_clone()?, upstream.try_clone()?);
    let up = std::thread::spawn(move || {
        let _ = io::copy(&mut from_client, &mut to_host);
        let _ = rustix::net::shutdown(&to_host, rustix::net::Shutdown::Write);
    });
    let (mut from_host, mut to_client) = (upstream, client);
    let _ = io::copy(&mut from_host, &mut to_client);
    let _ = to_client.shutdown(std::net::Shutdown::Write);
    let _ = up.join();
    Ok(())
}
