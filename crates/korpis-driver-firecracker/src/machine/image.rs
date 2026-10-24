// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block device's backup, browsed (§5.4 of docs/06-storage.md): the node has the helper restore
//! its image into a staging place of its own, and the driver serves a read-only file session on
//! that copy from a service guest of the copy's own ([`super::service`]), at [`AT`] in the
//! session. The guest's kernel parses the image and the host's never does. The copy is attached
//! writable, so the journal of an image taken from a running guest is replayed into the copy
//! rather than refusing the mount, and mounted read-only, and the worker serves it read-only. The
//! guest holds the copy and not the workload's volume, so it runs whether or not the workload's
//! own guest does; one serves every session on the same copy, and ends with the last.

use super::files::{Session, spec};
use super::service::{Wanted, service_id};
use super::{Machine, MachineResult, failed};
use crate::{agent, jail};
use buffa::Message as _;
use korpis_guest::config::Role;
use korpis_proto::korpis::driver::v1 as driver;

/// Where the backup's files are in the session.
pub const AT: &str = "/backup";

/// What a staged image's service guest is held by: never a workload's name, which has no colon.
fn key(image: &str) -> String {
    format!("image:{image}")
}

/// The spec of a service guest of `image`: the copy at [`AT`], read-only, and nothing else.
fn prepared(image: &driver::StagedImage) -> driver::PreparedSpec {
    driver::PreparedSpec {
        workload: image.workload.clone(),
        uid_base: image.uid_base,
        uid_count: image.uid_count,
        mounts: vec![driver::Mount {
            source: image.image.clone(),
            destination: AT.into(),
            volume: "backup".into(),
            read_only: true,
            ..Default::default()
        }],
        ..Default::default()
    }
}

impl Machine {
    /// A read-only file session on the staged backup image `image`.
    ///
    /// # Errors
    /// The image holds a filesystem its guests do not mount, is not a block device's beneath the
    /// volume root, or its ids are not a workload's; or its service guest could not be booted or
    /// did not take the session.
    pub async fn image_files(&self, image: &driver::StagedImage) -> MachineResult<Session> {
        if !crate::describe::GUEST_FILESYSTEMS.contains(&image.filesystem.as_str()) {
            let why = format!(
                "its guests mount {:?} alone, and the image holds {:?}: its backup is restored whole",
                crate::describe::GUEST_FILESYSTEMS,
                image.filesystem
            );
            return Err(super::MachineError::Refused(why));
        }
        super::create::ids(image.uid_base, image.uid_count)?;
        let root = self.settings.volume_root.canonicalize().map_err(failed)?;
        crate::volume::checked(&root, &image.image).map_err(super::MachineError::Refused)?;
        let prepared = prepared(image);
        let key = key(&image.image);
        let mut running = self.services.0.lock().await;
        let number = if let Some(held) = running.held.get_mut(&key) {
            held.sessions += 1;
            held.number
        } else {
            let wanted =
                Wanted { key: &key, spec: &prepared, role: Role::Files, content: None, copy: true };
            let vm = self.boot(&wanted).await?;
            running.hold(&key, vm, 1, None)
        };
        drop(running);
        let lease = self.lease(&key, number);
        let socket = jail::vsock_socket(&self.settings, &service_id(&key));
        let opened = agent::files(&socket, &spec(&prepared, false).encode_to_vec()).await;
        Ok(Session { stream: opened.map_err(failed)?, lease: Some(lease) })
    }
}
