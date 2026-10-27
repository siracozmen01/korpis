// SPDX-License-Identifier: AGPL-3.0-or-later

//! An RBD class's device: a Ceph image mapped through the kernel's RBD interface, by writing the
//! monitors, the user and its key, the pool, and the image to `/sys/bus/rbd/add_single_major`,
//! the kernel then reading and writing the image as `/dev/rbdN`; letting go of it writes `N` to
//! `remove_single_major`. The write is split on spaces and commas by the kernel, so each part is
//! checked to hold neither, the key to be base64. An image mapped already is found by the pool
//! and name the kernel reports for each device. What it costs: the key is in the kernel's options
//! for the mapping, which only root reads.

use crate::error::{HelperError, HelperResult};
use std::path::{Path, PathBuf};

const ADD: &str = super::RBD_ADD;
const REMOVE: &str = "/sys/bus/rbd/remove_single_major";
const DEVICES: &str = "/sys/bus/rbd/devices";

/// An RBD target as a provider answers it: `<monitors>|<pool>|<image>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    monitors: String,
    pool: String,
    image: String,
}

impl Target {
    /// The target `s` says, each part checked.
    pub(super) fn parse(s: &str) -> HelperResult<Self> {
        let mut parts = s.split('|');
        let (Some(monitors), Some(pool), Some(image), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(HelperError::Refused(format!(
                "RBD target {s:?}: <monitors>|<pool>|<image>"
            )));
        };
        super::plain("Ceph monitors", monitors, ",:[]")?;
        super::plain("RBD pool", pool, "")?;
        super::plain("RBD image", image, "")?;
        Ok(Self { monitors: monitors.into(), pool: pool.into(), image: image.into() })
    }

    /// What mapping it presenting `credential`, `<user>:<key>`, writes to the kernel.
    fn add(&self, credential: &str) -> HelperResult<String> {
        let refused = || HelperError::Refused("an RBD credential is <user>:<base64 key>".into());
        let (user, key) = credential.split_once(':').ok_or_else(refused)?;
        super::plain("Ceph user", user, "")?;
        let base64 = key.chars().all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c));
        if key.is_empty() || key.len() > 255 || !base64 {
            return Err(refused());
        }
        Ok(format!("{} name={user},secret={key} {} {}", self.monitors, self.pool, self.image))
    }
}

/// The number of the device mapping `t`, if one does.
fn mapped(t: &Target) -> std::io::Result<Option<String>> {
    let entries = match std::fs::read_dir(DEVICES) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let read = |f: &str| std::fs::read_to_string(entry.path().join(f)).unwrap_or_default();
        if read("pool").trim_end() == t.pool && read("name").trim_end() == t.image {
            return Ok(entry.file_name().to_str().map(str::to_owned));
        }
    }
    Ok(None)
}

/// Maps `t` presenting `credential`, or finds it mapped; the device.
pub(super) fn attach(t: &Target, credential: &str) -> HelperResult<PathBuf> {
    if mapped(t)?.is_none() {
        let add = t.add(credential)?;
        std::fs::write(ADD, add).map_err(|e| HelperError::Failed(format!("mapping RBD: {e}")))?;
    }
    let number = mapped(t)?.ok_or_else(|| HelperError::Failed("RBD image not mapped".into()))?;
    Ok(Path::new("/dev").join(format!("rbd{number}")))
}

/// Unmaps `t`, if it is mapped.
pub(super) fn release(t: &Target) -> HelperResult<()> {
    if let Some(number) = mapped(t)? {
        std::fs::write(REMOVE, number)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mapping_holds_nothing_the_kernel_would_split_otherwise() {
        let t = Target::parse("10.0.0.1:6789,10.0.0.2:6789|volumes|vol-7").unwrap();
        let add = t.add("korpis:QUJDRA==").unwrap();
        assert_eq!(add, "10.0.0.1:6789,10.0.0.2:6789 name=korpis,secret=QUJDRA== volumes vol-7");
        for bad in ["m|p", "m|p o|i", "m|p|i,x", "m m|p|i", "m|p|i|j"] {
            assert!(Target::parse(bad).is_err(), "{bad:?}");
        }
        for bad in ["korpis", "k:a b", "k,o:key", "k:key,secret=x", ":key"] {
            assert!(t.add(bad).is_err(), "{bad:?}");
        }
    }
}
