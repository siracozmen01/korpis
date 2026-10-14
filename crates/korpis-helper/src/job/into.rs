// SPDX-License-Identifier: AGPL-3.0-or-later

//! The directory a restore of one path is made into (§5.4 of docs/06-storage.md).

use crate::error::{HelperError, HelperResult, identifier};
use std::path::{Path, PathBuf};

/// Makes `into` at the root of the volume at `volume`, owned and permitted as the root is, so the
/// workload reaches what is restored there; it must not exist.
pub fn make_into(volume: &Path, into: &str) -> HelperResult<PathBuf> {
    use rustix::fs::{Mode, OFlags};
    identifier("restore directory", into)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = rustix::fs::open(volume, flags, Mode::empty()).map_err(std::io::Error::from)?;
    let st = rustix::fs::fstat(&root).map_err(std::io::Error::from)?;
    match rustix::fs::mkdirat(&root, into, Mode::from_raw_mode(0o700)) {
        Ok(()) => {}
        Err(rustix::io::Errno::EXIST) => {
            return Err(HelperError::Refused(format!("{into} already exists in the volume")));
        }
        Err(e) => return Err(std::io::Error::from(e).into()),
    }
    // Through a descriptor of what was made: a running workload may put a link in its place.
    let made =
        rustix::fs::openat(&root, into, flags, Mode::empty()).map_err(std::io::Error::from)?;
    let (uid, gid) = (
        rustix::fs::Uid::from_raw_unchecked(st.st_uid),
        rustix::fs::Gid::from_raw_unchecked(st.st_gid),
    );
    rustix::fs::fchown(&made, Some(uid), Some(gid)).map_err(std::io::Error::from)?;
    rustix::fs::fchmod(&made, Mode::from_raw_mode(st.st_mode & 0o7777))
        .map_err(std::io::Error::from)?;
    Ok(volume.join(into))
}
