// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file session in a guest (§4.4 of docs/04-runtimes.md): the guest's first process runs the
//! file worker its boot drive carries, as the node's helper runs it beside a container, and the
//! driver opens the session on the guest's files port and sends the worker's spec as its first
//! frame. The spec names each volume at the path the guest mounts it at, which is where the worker
//! finds it, and maps the guest's own ids, which are the workload's; the worker builds its root
//! on an empty directory of the guest's `/run`. The driver reads nothing of the session after.

use super::{Machine, MachineError, MachineResult, failed};
use crate::{agent, jail, process};
use buffa::Message as _;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::helper::v1 as helper;
use tokio::net::UnixStream;

/// Where the worker builds its root, in its own mount namespace in the guest.
const ROOT: &str = "/run/.korpis-files";

impl Machine {
    /// A file session on the volumes of object `id`'s running guest, writing those the workload
    /// writes if `write`: the connection to its worker, the spec sent.
    ///
    /// # Errors
    /// The guest does not run, has not started, or its files port did not take the session.
    pub async fn files(&self, id: &str, write: bool) -> MachineResult<UnixStream> {
        let object = self.load(id)?;
        if !object.vm.as_ref().is_some_and(process::alive) {
            return Err(MachineError::Refused(format!("{id} does not run")));
        }
        self.until_started(id).await?;
        let spec = spec(&object.spec, write).encode_to_vec();
        agent::files(&jail::vsock_socket(&self.settings, id), &spec).await.map_err(failed)
    }
}

/// The worker's spec for a guest made from `prepared`.
fn spec(prepared: &driver::PreparedSpec, write: bool) -> helper::FileWorkerSpec {
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
