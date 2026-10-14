// SPDX-License-Identifier: AGPL-3.0-or-later

//! Waking a following reader when its stream is written (§4 of docs/14-streams.md). The stream's
//! live tier is the segment being written, in the page cache; a follower is woken by inotify on
//! the stream's directory as soon as the writer appends to it, so a record reaches it moments
//! after it is written, and the writer has no second consumer to serve: it never learns anyone
//! is reading.

use rustix::fs::inotify::{self, CreateFlags, WatchFlags};
use rustix::io::Errno;
use std::os::fd::OwnedFd;
use std::path::Path;
use tokio::io::unix::AsyncFd;

/// A watch on one stream's directory.
#[derive(Debug)]
pub struct Wake {
    fd: AsyncFd<OwnedFd>,
}

impl Wake {
    /// Watches `dir` for records appended, segments begun, and segments sealed.
    pub fn watch(dir: &Path) -> std::io::Result<Self> {
        let fd = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)?;
        let what = WatchFlags::MODIFY | WatchFlags::CREATE | WatchFlags::MOVED_TO;
        inotify::add_watch(&fd, dir, what)?;
        Ok(Self { fd: AsyncFd::new(fd)? })
    }

    /// Resolves once the directory has changed since the last call.
    pub async fn changed(&self) -> std::io::Result<()> {
        let mut ready = self.fd.readable().await?;
        let mut events = [0u8; 4096];
        loop {
            match rustix::io::read(ready.get_inner(), &mut events) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(Errno::AGAIN) => {
                    ready.clear_ready();
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_write_to_the_directory_wakes_the_watcher() {
        let dir = tempfile::tempdir().unwrap();
        let wake = Wake::watch(dir.path()).unwrap();
        let quiet = tokio::time::timeout(std::time::Duration::from_millis(100), wake.changed());
        assert!(quiet.await.is_err(), "nothing written, nothing heard");
        std::fs::write(dir.path().join("00000000000000000000.open"), b"x").unwrap();
        let heard = tokio::time::timeout(std::time::Duration::from_secs(2), wake.changed());
        assert!(heard.await.is_ok(), "a write is heard at once");
    }
}
