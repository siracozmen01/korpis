// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's devices (§6.1 of docs/05-scheduling.md): those the Container Device Interface's
//! spec files on it describe, read with `korpis_cdi` when the agent starts and again every half
//! minute, so a file the vendor's toolkit rewrites reaches the node's facts by its next
//! heartbeat; and the devices a placement holds, given to the driver by id, class, and mode,
//! whose edits the driver reads itself (§4.2 of docs/04-runtimes.md). The agent never writes
//! the files.

mod classify;

use korpis_core::node::driver::DeviceClass;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// How often the spec directories are read again.
const EVERY: Duration = Duration::from_secs(30);

/// Where the devices are described, and what the operator says of them (§2 of
/// docs/18-operations.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// The spec directories, a later one's device replacing an earlier one's of the same name.
    pub dirs: Vec<PathBuf>,
    /// The class of a kind whose name is none Korpis knows, by kind (`vendor.com/kind`).
    pub classes: BTreeMap<String, DeviceClass>,
    /// How many workloads may hold a device of a kind at once in `timeslice`, by kind; a kind
    /// not named offers `passthrough` alone.
    pub shares: BTreeMap<String, u32>,
    /// Who must own the directories, the files, and the hooks' programs: root on a node.
    pub owner: u32,
}

/// What the node's spec files describe.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    /// The devices it offers, by id, as its facts carry them.
    pub devices: Vec<pb::Device>,
    /// The devices, files, and directories it does not offer, and why.
    pub unoffered: Vec<pb::UnofferedDevice>,
}

/// The node's devices as the spec files describe them now.
pub type Devices = watch::Receiver<Arc<Catalog>>;

/// What `config`'s directories describe now.
#[must_use]
pub fn read(config: &Config) -> Catalog {
    classify::catalog(korpis_cdi::describe(&config.dirs, config.owner), config)
}

/// How many of the devices `holds` names are of each class, as `catalog` describes each; one it
/// does not describe has no class, and is not counted.
#[must_use]
pub fn classes(catalog: Option<&Catalog>, holds: &[pb::DeviceHold]) -> Vec<(pb::DeviceClass, u64)> {
    let mut by_class: Vec<(pb::DeviceClass, u64)> = Vec::new();
    let described = |id: &str| catalog?.devices.iter().find(|d| d.id == id)?.class.as_known();
    for class in holds.iter().filter_map(|h| described(&h.device)) {
        match by_class.iter_mut().find(|(k, _)| *k == class) {
            Some((_, n)) => *n += 1,
            None => by_class.push((class, 1)),
        }
    }
    by_class
}

/// The devices `holds` name, as the driver is given them: each the catalog's device of that id,
/// with its class, in the mode it is held.
///
/// # Errors
/// A device held that the node no longer offers, which no driver can give.
pub fn given(
    catalog: &Catalog,
    holds: &[pb::DeviceHold],
) -> Result<Vec<driver::GivenDevice>, String> {
    holds
        .iter()
        .map(|h| {
            let d = catalog.devices.iter().find(|d| d.id == h.device).ok_or_else(|| {
                format!("the device {} it holds is no longer offered by this node", h.device)
            })?;
            Ok(driver::GivenDevice {
                id: d.id.clone(),
                class: d.class,
                mode: h.mode,
                ..Default::default()
            })
        })
        .collect()
}

/// What `config`'s directories describe, read now and every half minute after by the task
/// returned, which ends when every receiver has.
pub async fn watch(config: Config) -> (watch::Receiver<Arc<Catalog>>, JoinHandle<()>) {
    let first = blocking(config.clone()).await;
    let (send, receive) = watch::channel(Arc::new(first));
    let task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(EVERY).await;
            if send.is_closed() {
                return;
            }
            let now = blocking(config.clone()).await;
            send.send_if_modified(|held| replaced(held, now));
        }
    });
    (receive, task)
}

/// Whether `now` differs from `held`, which it then replaces.
fn replaced(held: &mut Arc<Catalog>, now: Catalog) -> bool {
    if **held == now {
        return false;
    }
    tracing::info!(devices = now.devices.len(), "the node's devices changed");
    *held = Arc::new(now);
    true
}

/// [`read`], off the runtime's threads.
async fn blocking(config: Config) -> Catalog {
    tokio::task::spawn_blocking(move || read(&config)).await.unwrap_or_else(|e| {
        tracing::error!(error = %e, "reading the node's devices failed; offering none");
        Catalog::default()
    })
}

/// A kind's class, as `--device-class` gives it: `vendor.com/kind=class`.
///
/// # Errors
/// It is not that form, names no class, or maps a kind whose own name is a class, which the
/// kind's name settles (§6.1 of docs/05-scheduling.md).
pub fn parse_class(s: &str) -> Result<(String, DeviceClass), String> {
    let (kind, class) = kind_and(s)?;
    let named = kind.split_once('/').and_then(|(_, n)| DeviceClass::named(n));
    if let Some(c) = named {
        return Err(format!("{kind} is a {} by its name already", c.as_str()));
    }
    let class = DeviceClass::named(class)
        .ok_or_else(|| format!("{class:?} is not a class: gpu, usb, pci, tpm, or serial"))?;
    Ok((kind.to_owned(), class))
}

/// A kind's shares, as `--device-share` gives them: `vendor.com/kind=N`, at least 1.
///
/// # Errors
/// It is not that form, or the count is not a positive number.
pub fn parse_share(s: &str) -> Result<(String, u32), String> {
    let (kind, n) = kind_and(s)?;
    match n.parse::<u32>() {
        Ok(n @ 1..) => Ok((kind.to_owned(), n)),
        _ => Err(format!("{n:?} is not a number of shares, at least 1")),
    }
}

/// A kind and what follows its `=`.
fn kind_and(s: &str) -> Result<(&str, &str), String> {
    let (kind, rest) = s.split_once('=').ok_or_else(|| format!("{s:?} is not kind=value"))?;
    if korpis_core::node::device::is_cdi_name(&format!("{kind}=x")) {
        Ok((kind, rest))
    } else {
        Err(format!("{kind:?} is not a CDI kind, vendor.com/kind"))
    }
}

#[cfg(test)]
mod tests;
