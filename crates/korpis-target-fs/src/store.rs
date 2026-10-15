// SPDX-License-Identifier: AGPL-3.0-or-later

//! The objects, as files beneath `objects/` in the provider's volume, each named by its key. A
//! put is written beside them in `incoming/` and renamed into place, so a reader sees a whole
//! object or none. A listing walks the tree in key order, which is the order of names with a
//! directory's taken as ending in `/`, and skips every directory whose keys all sort before
//! where it starts, so a page costs what it returns.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The longest key.
const LONGEST: usize = 1024;

/// One object in a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// Its key.
    pub key: String,
    /// Its size.
    pub size: u64,
    /// When it was written.
    pub modified: SystemTime,
}

/// A page of a listing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    /// The objects, in key order.
    pub objects: Vec<Listed>,
    /// Whether more follow.
    pub more: bool,
}

/// The objects of one volume.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

/// Whether `key` names an object: relative, at most 1024 bytes, with no empty, `.`, or `..`
/// component and no NUL.
#[must_use]
pub fn is_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= LONGEST
        && !key.contains('\0')
        && key.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

fn invalid(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, what.to_owned())
}

impl Store {
    /// The store in `root`, made if it is not there.
    ///
    /// # Errors
    /// Its directories cannot be made.
    pub fn open(root: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(root.join("objects"))?;
        std::fs::create_dir_all(root.join("incoming"))?;
        Ok(Self { root: root.to_owned() })
    }

    fn path(&self, key: &str) -> std::io::Result<PathBuf> {
        if !is_key(key) {
            return Err(invalid("not an object name"));
        }
        Ok(self.root.join("objects").join(key))
    }

    /// Writes `bytes` as `key`, replacing what it held.
    ///
    /// # Errors
    /// The key is not one, or the disk refuses.
    pub fn put(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write as _;
        let path = self.path(key)?;
        let mut name = [0u8; 12];
        getrandom::fill(&mut name).map_err(std::io::Error::other)?;
        let staged = self.root.join("incoming").join(crate::hex(&name));
        let mut f = std::fs::File::create(&staged)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&staged, &path)
    }

    /// The bytes of `key`, if it is there.
    ///
    /// # Errors
    /// The key is not one, or the disk refuses.
    pub fn get(&self, key: &str) -> std::io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(key)?) {
            Ok(b) => Ok(Some(b)),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::IsADirectory
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Deletes `keys`; how many were there. Directories left empty go with them.
    ///
    /// # Errors
    /// A key is not one, or the disk refuses.
    pub fn delete(&self, keys: &[String]) -> std::io::Result<u32> {
        let mut deleted = 0;
        for key in keys {
            let path = self.path(key)?;
            match std::fs::remove_file(&path) {
                Ok(()) => deleted += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            }
            self.tidy(&path);
        }
        Ok(deleted)
    }

    /// Removes the directories above `path` that its deletion left empty.
    fn tidy(&self, path: &Path) {
        let objects = self.root.join("objects");
        let mut dir = path.parent();
        while let Some(d) = dir.filter(|d| *d != objects) {
            // A directory still holding something is not removed, and stops the climb.
            if std::fs::remove_dir(d).is_err() {
                return;
            }
            dir = d.parent();
        }
    }

    /// Up to `limit` keys beginning with `prefix` and sorting after `after`, in key order.
    ///
    /// # Errors
    /// The disk refuses.
    pub fn list(&self, prefix: &str, after: &str, limit: usize) -> std::io::Result<Page> {
        // The walk starts at the deepest directory every matching key lies in.
        let start = prefix.rfind('/').map_or("", |i| &prefix[..=i]);
        if !start.is_empty() && !is_key(start.trim_end_matches('/')) {
            return Err(invalid("not a prefix"));
        }
        let mut walk = Walk { prefix, after, limit, found: Vec::new() };
        walk.dir(&self.root.join("objects").join(start), start)?;
        let more = walk.found.len() > limit;
        walk.found.truncate(limit);
        Ok(Page { objects: walk.found, more })
    }
}

/// A listing in progress.
struct Walk<'a> {
    prefix: &'a str,
    after: &'a str,
    limit: usize,
    found: Vec<Listed>,
}

impl Walk<'_> {
    /// Walks `dir`, whose keys begin with `base`; whether the page is full.
    fn dir(&mut self, dir: &Path, base: &str) -> std::io::Result<bool> {
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        let mut entries = Vec::new();
        for entry in read {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else { continue };
            let is_dir = entry.file_type()?.is_dir();
            entries.push((if is_dir { format!("{name}/") } else { name }, entry.path()));
        }
        entries.sort();
        for (name, path) in entries {
            if self.entry(format!("{base}{name}"), &path)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Takes the entry `key` at `path`, a directory's ending in `/`; whether the page is full.
    fn entry(&mut self, key: String, path: &Path) -> std::io::Result<bool> {
        if key.ends_with('/') {
            let relevant = self.prefix.starts_with(&key) || key.starts_with(self.prefix);
            // Every key beneath sorts before `after` when `after` is past `key` and not in it.
            let passed = *self.after > *key && !self.after.starts_with(&key);
            return if relevant && !passed { self.dir(path, &key) } else { Ok(false) };
        }
        if !key.starts_with(self.prefix) || *key <= *self.after {
            return Ok(false);
        }
        let meta = std::fs::metadata(path)?;
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        self.found.push(Listed { key, size: meta.len(), modified });
        Ok(self.found.len() > self.limit)
    }
}

#[cfg(test)]
mod tests;
