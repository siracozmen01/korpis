// SPDX-License-Identifier: AGPL-3.0-or-later

//! Unpacking an archive for `extract` (§4 of docs/09-recipes.md): tar, bare or in gzip, zstd, or
//! xz, and zip, recognised by their first bytes. An entry with an absolute path or a `..`
//! component is refused, as is a device, a FIFO, a link that would point out of the tree being
//! unpacked, and a hard link to anything not unpacked with it; nothing is unpacked for an archive
//! that holds one. `strip` drops leading components and `include` keeps only the entries at or
//! beneath the paths it names, both read after normalising. Entries are created by the worker,
//! so they belong to the workload's root, whatever owner an archive records.

use super::beneath;
use super::files;
use rustix::fs::{AtFlags, linkat, symlinkat};
use std::fs::File;
use std::io::{Read, Seek as _, SeekFrom};
use std::os::fd::OwnedFd;

/// How an archive is unpacked.
#[derive(Debug, Clone, Copy)]
pub struct Options<'a> {
    /// Leading components dropped.
    pub strip: u32,
    /// Only entries at or beneath these, when any are named.
    pub include: &'a [String],
}

/// What one entry is.
enum Kind<'r> {
    File { mode: u32, data: &'r mut dyn Read },
    Dir { mode: u32 },
    Symlink(String),
    Hardlink(String),
}

/// Where an entry named `raw` goes in the tree: normalised, stripped, and included, or `None`
/// when it is left out.
fn placed(raw: &str, o: Options<'_>) -> Result<Option<String>, String> {
    if raw.starts_with('/') || raw.contains('\0') {
        return Err(format!("{raw:?} is an absolute path"));
    }
    let parts: Vec<&str> = raw.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.contains(&"..") {
        return Err(format!("{raw:?} climbs out of the tree"));
    }
    let strip = usize::try_from(o.strip).unwrap_or(usize::MAX);
    if parts.len() <= strip {
        return Ok(None);
    }
    let rest = parts.get(strip..).unwrap_or_default().join("/");
    let kept = o.include.is_empty()
        || o.include.iter().any(|i| {
            let i = i.trim_matches('/');
            rest == i || rest.strip_prefix(i).is_some_and(|r| r.starts_with('/'))
        });
    Ok(kept.then_some(rest))
}

/// Whether a link at `at` in the tree, pointing at `target`, points inside the tree.
fn stays(at: &str, target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') {
        return false;
    }
    let mut depth: Vec<&str> = at.split('/').collect();
    depth.pop();
    target.split('/').all(|part| match part {
        "" | "." => true,
        ".." => depth.pop().is_some(),
        p => {
            depth.push(p);
            true
        }
    })
}

/// Where entries are unpacked, and the modes of the directories, set once all are unpacked.
struct Sink<'a> {
    root: &'a OwnedFd,
    dirs: Vec<(String, u32)>,
    count: u64,
}

impl Sink<'_> {
    fn put(&mut self, rel: &str, kind: Kind<'_>, o: Options<'_>) -> Result<(), String> {
        let (dir, name) = beneath::parent(self.root, rel, true)?;
        let failed = |e: std::io::Error| format!("{rel}: {e}");
        match kind {
            Kind::File { mode, data } => {
                files::remove(&dir, &name).map_err(failed)?;
                files::replace_file(&dir, &name, data, mode & 0o7777).map_err(failed)?;
            }
            Kind::Dir { mode } => {
                files::make_dir(&dir, &name).map_err(failed)?;
                self.dirs.push((rel.to_owned(), mode & 0o7777));
            }
            Kind::Symlink(target) => {
                if !stays(rel, &target) {
                    return Err(format!("{rel} links to {target:?}, outside what is unpacked"));
                }
                files::remove(&dir, &name).map_err(failed)?;
                symlinkat(target.as_str(), &dir, name.as_str()).map_err(|e| failed(e.into()))?;
            }
            Kind::Hardlink(target) => {
                let to = placed(&target, o)?
                    .ok_or_else(|| format!("{rel} links to {target:?}, which is not unpacked"))?;
                let (tdir, tname) = beneath::parent(self.root, &to, false)?;
                files::remove(&dir, &name).map_err(failed)?;
                linkat(&tdir, tname.as_str(), &dir, name.as_str(), AtFlags::empty())
                    .map_err(|e| failed(e.into()))?;
            }
        }
        self.count += 1;
        Ok(())
    }

    /// Sets the directories' modes, deepest first, now nothing more is made in them.
    fn finish(mut self) -> Result<u64, String> {
        self.dirs.sort_by_key(|(rel, _)| std::cmp::Reverse(rel.matches('/').count()));
        for (rel, mode) in &self.dirs {
            let (dir, name) = beneath::parent(self.root, rel, false)?;
            files::set_mode(&dir, &name, *mode).map_err(|e| format!("{rel}: {e}"))?;
        }
        Ok(self.count)
    }
}

/// The compression or container `file` begins with.
enum Format {
    Zip,
    Gzip,
    Zstd,
    Xz,
    Tar,
}

fn format(file: &mut File) -> std::io::Result<Format> {
    let mut magic = [0u8; 6];
    let n = file.read(&mut magic)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(match magic.get(..n).unwrap_or_default() {
        [0x50, 0x4b, 0x03, 0x04, ..] | [0x50, 0x4b, 0x05, 0x06, ..] => Format::Zip,
        [0x1f, 0x8b, ..] => Format::Gzip,
        [0x28, 0xb5, 0x2f, 0xfd, ..] => Format::Zstd,
        [0xfd, b'7', b'z', b'X', b'Z', 0x00] => Format::Xz,
        _ => Format::Tar,
    })
}

/// Unpacks `file` into the directory `into`; how many entries it unpacked.
///
/// # Errors
/// The archive is malformed, or holds an entry that is refused (above).
pub fn unpack(mut file: File, into: &OwnedFd, o: Options<'_>) -> Result<u64, String> {
    let mut sink = Sink { root: into, dirs: Vec::new(), count: 0 };
    let io = |e: std::io::Error| e.to_string();
    match format(&mut file).map_err(io)? {
        Format::Zip => unzip(file, &mut sink, o)?,
        Format::Gzip => untar(flate2::read::MultiGzDecoder::new(file), &mut sink, o)?,
        Format::Zstd => untar(zstd::stream::read::Decoder::new(file).map_err(io)?, &mut sink, o)?,
        Format::Xz => unxz(file, &mut sink, o)?,
        Format::Tar => untar(file, &mut sink, o)?,
    }
    sink.finish()
}

/// xz is decoded on a thread of its own into a pipe the tar reader reads.
fn unxz(file: File, sink: &mut Sink<'_>, o: Options<'_>) -> Result<(), String> {
    let (reader, mut writer) = std::io::pipe().map_err(|e| e.to_string())?;
    let decoding = std::thread::spawn(move || {
        let mut input = std::io::BufReader::new(file);
        lzma_rs::xz_decompress(&mut input, &mut writer)
    });
    let unpacked = untar(reader, sink, o);
    let decoded = decoding.join().map_err(|_| "the xz decoder failed".to_owned())?;
    unpacked?;
    decoded.map_err(|e| format!("xz: {e}"))
}

fn untar(read: impl Read, sink: &mut Sink<'_>, o: Options<'_>) -> Result<(), String> {
    use tar::EntryType as T;
    let mut archive = tar::Archive::new(read);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut e = entry.map_err(|e| e.to_string())?;
        let raw = String::from_utf8(e.path_bytes().into_owned())
            .map_err(|_| "an entry's path is not UTF-8".to_owned())?;
        let ty = e.header().entry_type();
        if matches!(ty, T::XGlobalHeader) {
            continue;
        }
        let mode = e.header().mode().unwrap_or(0o644);
        let link = || -> Result<String, String> {
            let l = e.link_name_bytes().ok_or("a link names no target")?;
            String::from_utf8(l.into_owned()).map_err(|_| "a link's target is not UTF-8".into())
        };
        let kind_of = match ty {
            T::Regular | T::Continuous => None,
            T::Directory => Some(Kind::Dir { mode }),
            T::Symlink => Some(Kind::Symlink(link()?)),
            T::Link => Some(Kind::Hardlink(link()?)),
            other => return Err(format!("{raw:?} is a {other:?}, which is not unpacked")),
        };
        let Some(rel) = placed(&raw, o)? else { continue };
        let kind = kind_of.unwrap_or(Kind::File { mode, data: &mut e });
        sink.put(&rel, kind, o)?;
    }
    Ok(())
}

fn unzip(file: File, sink: &mut Sink<'_>, o: Options<'_>) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        let raw = f.name().map_err(|e| e.to_string())?.into_owned();
        let mode = f.unix_mode().unwrap_or(if f.is_dir() { 0o755 } else { 0o644 });
        let Some(rel) = placed(&raw, o)? else { continue };
        if f.is_dir() {
            sink.put(&rel, Kind::Dir { mode }, o)?;
        } else if f.is_symlink() {
            let mut target = String::new();
            f.read_to_string(&mut target).map_err(|e| e.to_string())?;
            sink.put(&rel, Kind::Symlink(target), o)?;
        } else if f.is_file() {
            sink.put(&rel, Kind::File { mode, data: &mut f }, o)?;
        } else {
            return Err(format!("{raw:?} is neither a file, a directory, nor a link"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Options, placed, stays};

    #[test]
    fn paths_are_normalised_stripped_and_included_and_escapes_refused() {
        let all = Options { strip: 0, include: &[] };
        assert_eq!(placed("./a//b/", all).unwrap().as_deref(), Some("a/b"));
        assert!(placed("/etc/passwd", all).is_err());
        assert!(placed("a/../../x", all).is_err());
        let strip = Options { strip: 1, include: &[] };
        assert_eq!(placed("server-1.0/run.sh", strip).unwrap().as_deref(), Some("run.sh"));
        assert_eq!(placed("server-1.0", strip).unwrap(), None);
        let only = ["bin".to_owned()];
        let some = Options { strip: 0, include: &only };
        assert_eq!(placed("bin/x", some).unwrap().as_deref(), Some("bin/x"));
        assert_eq!(placed("binary", some).unwrap(), None, "a prefix of a name is not a parent");
    }

    #[test]
    fn a_link_stays_in_the_tree_or_is_refused() {
        assert!(stays("a/b/link", "../c"));
        assert!(stays("link", "c/d"));
        assert!(!stays("a/link", "../../x"));
        assert!(!stays("link", "/etc/passwd"));
        assert!(!stays("link", ""));
    }
}
