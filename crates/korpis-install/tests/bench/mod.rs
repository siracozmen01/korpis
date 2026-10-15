// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the install tests share: a directory standing in for a volume, a content store holding
//! the recipe's files and what fetches downloaded, and a place outside both that nothing may reach.

#![allow(dead_code, reason = "each test binary uses part of the bench")]

use korpis_core::recipe::install::InstallStep;
use korpis_core::recipe::installing::InstallAction;
use korpis_install::{Content, Volumes, run};
use sha2::Digest as _;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

pub struct Bench {
    _dir: tempfile::TempDir,
    pub volume: PathBuf,
    /// The content store: the recipe's files in `blobs`, what fetches downloaded in `fetched`.
    pub blobs: PathBuf,
    pub fetched: PathBuf,
    pub outside: PathBuf,
    pub files: BTreeMap<String, String>,
}

impl Bench {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let at = |p: &str| dir.path().join(p);
        let (volume, blobs, fetched, outside) =
            (at("vol"), at("blobs"), at("fetched"), at("outside"));
        for d in [
            &volume,
            &blobs.join("sha256"),
            &fetched.join("sha256"),
            &fetched.join("sha512"),
            &outside,
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        Self { _dir: dir, volume, blobs, fetched, outside, files: BTreeMap::new() }
    }

    /// Keeps `bytes` in `dir` of the content store; their digest.
    fn put(dir: &Path, bytes: &[u8]) -> String {
        let hex = sha2::Sha256::digest(bytes).iter().fold(String::new(), |mut s, b| {
            let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
            s
        });
        std::fs::write(dir.join("sha256").join(&hex), bytes).unwrap();
        format!("sha256:{hex}")
    }

    /// Keeps `bytes` as a fetch downloaded them; their digest.
    pub fn keep(&self, bytes: &[u8]) -> String {
        Self::put(&self.fetched, bytes)
    }

    /// Makes `bytes` the recipe's file at `path`; their digest.
    pub fn file(&mut self, path: &str, bytes: &[u8]) -> String {
        let digest = Self::put(&self.blobs, bytes);
        self.files.insert(path.to_owned(), digest.clone());
        digest
    }

    fn volumes(&self) -> Volumes {
        Volumes::open(&[("/data".to_owned(), self.volume.as_path())]).unwrap()
    }

    fn content(&self) -> Content {
        let flags = rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY;
        let open = |d: &Path| rustix::fs::open(d, flags, rustix::fs::Mode::empty()).unwrap();
        Content {
            blobs: open(&self.blobs),
            fetched: open(&self.fetched),
            files: self.files.clone(),
        }
    }

    pub fn run(
        &self,
        actions: &[InstallAction],
        from: usize,
    ) -> Result<Vec<usize>, korpis_install::Failed> {
        let (volumes, content) = (self.volumes(), self.content());
        let mut done = Vec::new();
        let mut record = |i| {
            done.push(i);
            Ok(())
        };
        run(actions, from, &volumes, &content, &mut record)?;
        Ok(done)
    }

    pub fn at(&self, path: &str) -> PathBuf {
        self.volume.join(path.trim_start_matches("/data/"))
    }
}

pub fn act(step: InstallStep) -> InstallAction {
    InstallAction { step, context: None }
}

pub fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}
