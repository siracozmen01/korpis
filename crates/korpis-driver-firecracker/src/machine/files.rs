// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file session in a guest (§4.4 of docs/04-runtimes.md): the guest's first process runs the
//! file worker its boot drive carries, as the node's helper runs it beside a container, and the
//! driver opens the session on the guest's files port and sends the worker's spec as its first
//! frame. The spec names each volume at the path the guest mounts it at, which is where the worker
//! finds it, and maps the guest's own ids, which are the workload's; the worker builds its root
//! on an empty directory of the guest's `/run`. The driver reads nothing of the session after.
//! A stopped object's session is served the same way by a service guest ([`super::service`]).

use super::{Machine, MachineResult, failed};
use crate::{agent, jail, process};
use buffa::Message as _;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::helper::v1 as helper;
use tokio::net::UnixStream;

/// Where the worker builds its root, in its own mount namespace in the guest.
const ROOT: &str = "/run/.korpis-files";

/// A file session: the connection to its worker, and for a stopped object the hold on the guest
/// serving it, which is to be kept until the session ends.
#[derive(Debug)]
pub struct Session {
    /// The worker's end, its spec sent.
    pub stream: UnixStream,
    /// The hold on the service guest, for an object that does not run.
    pub lease: Option<super::service::Lease>,
}

impl Machine {
    /// A file session on the volumes of object `id`, writing those the workload writes if
    /// `write`: in its own guest while it runs, and in a service guest while it does not.
    ///
    /// # Errors
    /// The guest has not started, its files port did not take the session, or a service guest
    /// could not be booted.
    pub async fn files(&self, id: &str, write: bool) -> MachineResult<Session> {
        let object = self.load(id)?;
        let spec = spec(&object.spec, write).encode_to_vec();
        if object.vm.as_ref().is_some_and(process::alive) {
            self.until_started(id).await?;
            let socket = jail::vsock_socket(&self.settings, id);
            let stream = agent::files(&socket, &spec).await.map_err(failed)?;
            return Ok(Session { stream, lease: None });
        }
        let (socket, lease) = self.service(id).await?;
        let stream = agent::files(&socket, &spec).await.map_err(failed)?;
        Ok(Session { stream, lease: Some(lease) })
    }
}

/// The worker's spec for a guest made from `prepared`.
pub(super) fn spec(prepared: &driver::PreparedSpec, write: bool) -> helper::FileWorkerSpec {
    let volumes = prepared
        .mounts
        .iter()
        .map(|m| helper::FileVolume {
            path: m.destination.clone(),
            volume: m.volume.clone(),
            directory: m.destination.clone(),
            read_only: m.read_only,
            ..Default::default()
        })
        .collect();
    helper::FileWorkerSpec { root: ROOT.into(), id_base: 0, volumes, write, ..Default::default() }
}
