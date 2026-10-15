// SPDX-License-Identifier: AGPL-3.0-or-later

//! Running an install's actions in order (§4 of docs/09-recipes.md), each stating what running it
//! again does: content and literal text replace their target whole, an archive replaces its target
//! tree whole, a directory is made or has its mode corrected, a mode or an owner is set, a link
//! replaces a link and nothing else, a deletion of what is absent does nothing, and a template is
//! rendered from its source each time. `done` is told after each action, so a resume starts at the
//! action that failed.

use super::archive::{self, Options};
use super::beneath::{self, RESOLVE, Volumes};
use super::files;
use korpis_core::recipe::install::{FETCHED, InstallStep};
use korpis_core::recipe::installing::InstallAction;
use korpis_core::recipe::template::{MAX_SOURCE, Template};
use rustix::fs::{FileType, Mode, OFlags, openat2};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read as _;
use std::os::fd::OwnedFd;

/// The mode content, text, and templates are written with.
const FILE_MODE: u32 = 0o644;

/// What an install reads: the node's content store, read-only.
#[derive(Debug)]
pub struct Content {
    /// The store's `blobs` directory, where the recipe's files are: `sha256/<hex>`.
    pub blobs: OwnedFd,
    /// The store's `fetched` directory, where what a `fetch` downloaded is: `sha256/<hex>` and
    /// `sha512/<hex>`, and nothing pulled from a registry.
    pub fetched: OwnedFd,
    /// Each file of the recipe, by its path in the artifact, and its digest.
    pub files: BTreeMap<String, String>,
}

impl Content {
    /// What a `fetch` of `digest` downloaded. Only there: a digest a recipe names never reaches a
    /// recipe's file or an image layer, which may be another tenant's and private.
    fn fetched(&self, digest: &str) -> Result<File, String> {
        open_digest(&self.fetched, digest)
    }

    /// The recipe's file at `path` in the artifact.
    fn artifact(&self, path: &str) -> Result<File, String> {
        let path = path.strip_prefix("./").unwrap_or(path);
        let digest = self.files.get(path).ok_or_else(|| format!("the recipe holds no {path}"))?;
        open_digest(&self.blobs, digest)
    }
}

/// The content `digest` names in the directory `dir`.
fn open_digest(dir: &OwnedFd, digest: &str) -> Result<File, String> {
    let (alg, hex) = digest.split_once(':').ok_or_else(|| format!("{digest:?} is no digest"))?;
    if !matches!(alg, "sha256" | "sha512") || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{digest:?} is no digest"));
    }
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = openat2(dir, format!("{alg}/{hex}").as_str(), flags, Mode::empty(), RESOLVE)
        .map_err(|e| format!("{digest} is not in the content store: {e}"))?;
    Ok(File::from(fd))
}

/// An action that failed: which, and why.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("install action {index}: {reason}")]
pub struct Failed {
    /// Its place in the install, from zero.
    pub index: usize,
    /// What went wrong.
    pub reason: String,
}

/// Runs `actions` from `from` on, telling `done` the index of each one finished.
///
/// # Errors
/// The first action that fails, or `done` failing to record one.
pub fn run(
    actions: &[InstallAction],
    from: usize,
    volumes: &Volumes,
    content: &Content,
    done: &mut dyn FnMut(usize) -> std::io::Result<()>,
) -> Result<(), Failed> {
    for (index, a) in actions.iter().enumerate().skip(from) {
        let failed = |reason: String| Failed { index, reason };
        act(a, volumes, content).map_err(failed)?;
        done(index).map_err(|e| failed(format!("its progress was not recorded: {e}")))?;
    }
    Ok(())
}

/// Opens a workload path to read: a file beneath its volume, through links that stay there.
fn read_in(volumes: &Volumes, path: &str) -> Result<File, String> {
    let (_, root, rel) = volumes.locate(path)?;
    let flags = OFlags::RDONLY | OFlags::NOCTTY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = openat2(root, rel.as_str(), flags, Mode::empty(), RESOLVE)
        .map_err(|e| format!("{path}: {e}"))?;
    Ok(File::from(fd))
}

/// The directory holding `path` and its last component; the volume root itself names nothing.
fn target(volumes: &Volumes, path: &str, make: bool) -> Result<(OwnedFd, String), String> {
    let (_, root, rel) = volumes.locate(path)?;
    if rel.is_empty() {
        return Err(format!("{path} is a volume's root, which an install does not replace"));
    }
    beneath::parent(root, &rel, make)
}

fn write(volumes: &Volumes, to: &str, src: &mut dyn std::io::Read) -> Result<(), String> {
    let (dir, name) = target(volumes, to, true)?;
    files::replace_file(&dir, &name, src, FILE_MODE).map_err(|e| format!("{to}: {e}"))
}

/// A failure about `what`.
fn at(what: &str) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

fn act(a: &InstallAction, volumes: &Volumes, content: &Content) -> Result<(), String> {
    let io = at;
    match &a.step {
        InstallStep::Fetch { hash, to, .. } => {
            write(volumes, to, &mut content.fetched(&hash.to_digest())?)
        }
        InstallStep::Copy { from, to } => write(volumes, to, &mut content.artifact(from)?),
        InstallStep::Write { path, content: text, mode } => {
            let (dir, name) = target(volumes, path, true)?;
            files::replace_file(&dir, &name, &mut text.as_bytes(), *mode).map_err(io(path))
        }
        InstallStep::Template { from, to, format, .. } => {
            let vars = a.context.as_ref().ok_or("a template is run with its context")?;
            let mut src = String::new();
            let read = content.artifact(from)?.take(u64::try_from(MAX_SOURCE).unwrap_or(0) + 1);
            std::io::BufReader::new(read).read_to_string(&mut src).map_err(io(from))?;
            let rendered = Template::parse(&src)
                .and_then(|t| t.render(*format, vars))
                .map_err(|e| format!("{from}: {e}"))?;
            write(volumes, to, &mut rendered.as_bytes())
        }
        InstallStep::Extract { from, to, strip, include } => {
            let src = if let Some(digest) = from.strip_prefix(FETCHED) {
                // An archive a step provider answered, which the node fetched (§4 of
                // docs/09-recipes.md): read from what fetches downloaded, as a `fetch` reads.
                content.fetched(&digest.replacen('/', ":", 1))?
            } else if from.starts_with('/') {
                read_in(volumes, from)?
            } else {
                content.artifact(from)?
            };
            extract(volumes, src, to, Options { strip: *strip, include })
        }
        InstallStep::Mkdir { path, mode } => mkdir(volumes, path, *mode),
        InstallStep::Chmod { path, mode } => {
            let (dir, name) = existing(volumes, path)?;
            files::set_mode(&dir, &name, *mode).map_err(io(path))
        }
        InstallStep::Chown { path, uid, gid } => {
            let (dir, name) = existing(volumes, path)?;
            files::set_owner(&dir, &name, *uid, *gid).map_err(io(path))
        }
        InstallStep::Symlink { target: to, link } => symlink(volumes, to, link),
        InstallStep::Delete { path } => delete(volumes, path),
        InstallStep::Provider { provider, .. } => {
            Err(format!("{provider} is resolved into content before an install runs it"))
        }
    }
}

/// Removes `path`, all of it; nothing when it, or a directory above it, is absent. Any other
/// failure to reach it is a failure: a link out of the volume is not an absence.
fn delete(volumes: &Volumes, path: &str) -> Result<(), String> {
    let (_, root, rel) = volumes.locate(path)?;
    let (up, name) = rel.rsplit_once('/').unwrap_or(("", rel.as_str()));
    if !beneath::plain(name) {
        return Err(format!("{path} is a volume's root, which an install does not remove"));
    }
    match beneath::dir(root, up, false) {
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(e) => Err(format!("{path}: {e}")),
        Ok(dir) => files::remove(&dir, name).map_err(at(path)),
    }
}

/// `path` as a directory and a name, or the volume root as itself and `.`.
fn existing(volumes: &Volumes, path: &str) -> Result<(OwnedFd, String), String> {
    let (_, root, rel) = volumes.locate(path)?;
    if rel.is_empty() {
        let dir = beneath::dir(root, "", false).map_err(|e| format!("{path}: {e}"))?;
        return Ok((dir, ".".to_owned()));
    }
    beneath::parent(root, &rel, false)
}

fn mkdir(volumes: &Volumes, path: &str, mode: u32) -> Result<(), String> {
    let (_, root, rel) = volumes.locate(path)?;
    beneath::dir(root, &rel, true).map_err(|e| format!("{path}: {e}"))?;
    let (dir, name) = existing(volumes, path)?;
    files::set_mode(&dir, &name, mode).map_err(|e| format!("{path}: {e}"))
}

/// Unpacks into a fresh tree beside `to`, then swaps it for what `to` was and removes that: the
/// target becomes what the archive holds, never a merge of it with what was there.
fn extract(volumes: &Volumes, src: File, to: &str, o: Options<'_>) -> Result<(), String> {
    let (dir, name) = target(volumes, to, true)?;
    let fresh = files::temp_name("extract");
    files::make_dir(&dir, &fresh).map_err(|e| format!("{to}: {e}"))?;
    let unpacked = beneath::dir(&dir, &fresh, false)
        .map_err(|e| e.to_string())
        .and_then(|into| archive::unpack(src, &into, o));
    if let Err(e) = unpacked {
        let _ = files::remove(&dir, &fresh);
        return Err(format!("{to}: {e}"));
    }
    let swap = match beneath::kind(&dir, &name).map_err(|e| e.to_string())? {
        Some(_) => rustix::fs::renameat_with(
            &dir,
            fresh.as_str(),
            &dir,
            name.as_str(),
            rustix::fs::RenameFlags::EXCHANGE,
        ),
        None => rustix::fs::renameat(&dir, fresh.as_str(), &dir, name.as_str()),
    };
    swap.map_err(|e| format!("{to}: {e}"))?;
    files::remove(&dir, &fresh).map_err(|e| format!("{to}: the tree it replaced: {e}"))
}

/// A link at `link` to `to`, which must be in the same volume: an absolute workload path there,
/// or a relative path that stays in it. A link already there is replaced; anything else is not.
fn symlink(volumes: &Volumes, to: &str, link: &str) -> Result<(), String> {
    let (volume, _, rel) = volumes.locate(link)?;
    let inside = if to.starts_with('/') {
        volumes.locate(to).is_ok_and(|(v, _, _)| v == volume)
    } else {
        let mut depth: Vec<&str> = rel.split('/').collect();
        depth.pop();
        to.split('/').all(|p| match p {
            "" | "." => true,
            ".." => depth.pop().is_some(),
            p => {
                depth.push(p);
                true
            }
        })
    };
    if !inside {
        return Err(format!("{link} would link to {to}, outside its volume"));
    }
    let (dir, name) = target(volumes, link, true)?;
    match beneath::kind(&dir, &name).map_err(|e| e.to_string())? {
        None | Some(FileType::Symlink) => {}
        Some(_) => return Err(format!("{link} is there and is not a link: it is not replaced")),
    }
    let tmp = files::temp_name("link");
    rustix::fs::symlinkat(to, &dir, tmp.as_str()).map_err(|e| format!("{link}: {e}"))?;
    rustix::fs::renameat(&dir, tmp.as_str(), &dir, name.as_str()).map_err(|e| {
        let _ = rustix::fs::unlinkat(&dir, tmp.as_str(), rustix::fs::AtFlags::empty());
        format!("{link}: {e}")
    })
}
