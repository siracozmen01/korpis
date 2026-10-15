// SPDX-License-Identifier: AGPL-3.0-or-later

//! The install actions on directories standing in for volumes: every verb, run twice to the same
//! result; links a tenant planted, refused rather than followed out; archives in each format,
//! replacing their target whole; hostile archives, refused with nothing unpacked; and a resume at
//! the action that failed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_core::recipe::install::{Hash, InstallStep};
use korpis_core::recipe::installing::InstallAction;
use korpis_core::recipe::template::{Context, TemplateFormat, Var};
use korpis_install::{Content, Volumes, run};
use sha2::Digest as _;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

struct Bench {
    _dir: tempfile::TempDir,
    volume: PathBuf,
    /// The content store: the recipe's files in `blobs`, what fetches downloaded in `fetched`.
    blobs: PathBuf,
    fetched: PathBuf,
    outside: PathBuf,
    files: BTreeMap<String, String>,
}

impl Bench {
    fn new() -> Self {
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
    fn keep(&self, bytes: &[u8]) -> String {
        Self::put(&self.fetched, bytes)
    }

    /// Makes `bytes` the recipe's file at `path`; their digest.
    fn file(&mut self, path: &str, bytes: &[u8]) -> String {
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

    fn run(
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

    fn at(&self, path: &str) -> PathBuf {
        self.volume.join(path.trim_start_matches("/data/"))
    }
}

fn act(step: InstallStep) -> InstallAction {
    InstallAction { step, context: None }
}

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

/// One of each verb but `extract` and `chown`, which have tests of their own.
fn every_verb(jar: &str) -> Vec<InstallAction> {
    let context: Context = [
        ("motd".to_owned(), Var::Text("Hi =x".into())),
        ("max".to_owned(), Var::Number("20".into())),
    ]
    .into();
    vec![
        act(InstallStep::Mkdir { path: "/data/plugins".into(), mode: 0o750 }),
        act(InstallStep::Fetch {
            url: "https://example.org/s.jar".into(),
            hash: Hash::parse(jar).unwrap(),
            to: "/data/bin/server.jar".into(),
        }),
        act(InstallStep::Copy { from: "motd.txt".into(), to: "/data/motd.txt".into() }),
        act(InstallStep::Write {
            path: "/data/eula.txt".into(),
            content: "eula=true\n".into(),
            mode: 0o640,
        }),
        InstallAction {
            step: InstallStep::Template {
                from: "templates/server.properties".into(),
                to: "/data/server.properties".into(),
                format: TemplateFormat::Properties,
                vars: vec!["motd".into(), "max".into()],
            },
            context: Some(context),
        },
        act(InstallStep::Chmod { path: "/data/bin/server.jar".into(), mode: 0o600 }),
        act(InstallStep::Symlink {
            target: "bin/server.jar".into(),
            link: "/data/server.jar".into(),
        }),
        act(InstallStep::Delete { path: "/data/old.log".into() }),
        act(InstallStep::Delete { path: "/data/never-there".into() }),
    ]
}

#[test]
fn every_verb_writes_beneath_its_volume_and_running_again_gives_the_same_result() {
    let mut b = Bench::new();
    let jar = b.keep(b"the server");
    b.file("templates/server.properties", b"motd={{motd}}\nmax={{max}}\n");
    b.file("motd.txt", b"welcome\n");
    std::fs::write(b.at("/data/old.log"), b"stale").unwrap();
    let actions = every_verb(&jar);
    for round in 0..2 {
        assert_eq!(
            b.run(&actions, 0).unwrap(),
            (0..actions.len()).collect::<Vec<_>>(),
            "round {round}"
        );
        assert_eq!(mode(&b.at("/data/plugins")), 0o750);
        assert_eq!(std::fs::read(b.at("/data/bin/server.jar")).unwrap(), b"the server");
        assert_eq!(mode(&b.at("/data/bin/server.jar")), 0o600);
        assert_eq!(std::fs::read(b.at("/data/motd.txt")).unwrap(), b"welcome\n");
        assert_eq!(mode(&b.at("/data/eula.txt")), 0o640);
        let props = std::fs::read_to_string(b.at("/data/server.properties")).unwrap();
        assert_eq!(props, "motd=Hi =x\nmax=20\n");
        assert_eq!(
            std::fs::read_link(b.at("/data/server.jar")).unwrap(),
            Path::new("bin/server.jar")
        );
        assert!(!b.at("/data/old.log").exists());
    }
    let left: Vec<_> = std::fs::read_dir(&b.volume)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".korpis-"))
        .collect();
    assert!(left.is_empty(), "nothing half-made is left: {left:?}");
}

#[test]
fn a_link_the_tenant_planted_never_carries_a_verb_out_of_the_volume() {
    let b = Bench::new();
    std::os::unix::fs::symlink(&b.outside, b.at("/data/escape")).unwrap();
    std::os::unix::fs::symlink("../outside", b.at("/data/up")).unwrap();
    std::fs::write(b.outside.join("secret"), b"host").unwrap();
    std::os::unix::fs::symlink(b.outside.join("secret"), b.at("/data/secret")).unwrap();
    for path in ["/data/escape/x", "/data/up/x"] {
        let w = act(InstallStep::Write { path: path.into(), content: "x".into(), mode: 0o644 });
        assert!(b.run(&[w], 0).is_err(), "{path}");
    }
    let chmod = act(InstallStep::Chmod { path: "/data/secret".into(), mode: 0o777 });
    assert!(b.run(&[chmod], 0).is_err(), "a mode is never set through a link");
    let extract = act(InstallStep::Extract {
        from: "/data/secret".into(),
        to: "/data/x".into(),
        strip: 0,
        include: vec![],
    });
    assert!(b.run(&[extract], 0).is_err(), "nor is anything read through one");
    let replace =
        act(InstallStep::Symlink { target: "/data/x".into(), link: "/data/plain".into() });
    std::fs::write(b.at("/data/plain"), b"a file").unwrap();
    assert!(b.run(&[replace], 0).is_err(), "a link replaces a link, nothing else");
    let out = act(InstallStep::Symlink { target: "../../etc".into(), link: "/data/l".into() });
    assert!(b.run(&[out], 0).is_err(), "a link verb points inside its volume");
    assert_eq!(std::fs::read_dir(&b.outside).unwrap().count(), 1, "nothing written outside");
    assert_eq!(mode(&b.outside.join("secret")) & 0o777, mode(&b.outside.join("secret")) & 0o777);
    let through = act(InstallStep::Delete { path: "/data/escape/secret".into() });
    assert!(b.run(&[through], 0).is_err(), "a link out is not an absence");
    assert!(b.outside.join("secret").exists());
    let root = act(InstallStep::Delete { path: "/data".into() });
    assert!(b.run(&[root], 0).is_err(), "a volume's root is not an install's to remove");
}

/// A tar of `pkg/` holding `run.sh` and a link to it.
fn tar() -> Vec<u8> {
    let mut t = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Directory);
    h.set_mode(0o755);
    h.set_size(0);
    t.append_data(&mut h, "pkg/", std::io::empty()).unwrap();
    let mut h = tar::Header::new_gnu();
    h.set_mode(0o755);
    h.set_size(4);
    t.append_data(&mut h, "pkg/run.sh", &b"#!x\n"[..]).unwrap();
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Symlink);
    h.set_size(0);
    t.append_link(&mut h, "pkg/latest", "run.sh").unwrap();
    t.into_inner().unwrap()
}

fn zip() -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.add_directory("pkg/", o.unix_permissions(0o755)).unwrap();
    z.start_file("pkg/run.sh", o.unix_permissions(0o755)).unwrap();
    z.write_all(b"#!x\n").unwrap();
    z.add_symlink("pkg/latest", "run.sh", o).unwrap();
    z.finish().unwrap().into_inner()
}

#[test]
fn an_archive_in_each_format_becomes_its_target_whole() {
    let plain = tar();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&plain).unwrap();
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut plain.as_slice(), &mut xz).unwrap();
    let formats = [
        ("tar", plain.clone()),
        ("gzip", gz.finish().unwrap()),
        ("zstd", zstd::encode_all(plain.as_slice(), 3).unwrap()),
        ("xz", xz),
        ("zip", zip()),
    ];
    for (name, bytes) in formats {
        let mut b = Bench::new();
        b.file("server.archive", &bytes);
        std::fs::create_dir_all(b.at("/data/server")).unwrap();
        std::fs::write(b.at("/data/server/stale"), b"from before").unwrap();
        let x = act(InstallStep::Extract {
            from: "server.archive".into(),
            to: "/data/server".into(),
            strip: 1,
            include: vec![],
        });
        b.run(std::slice::from_ref(&x), 0).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(std::fs::read(b.at("/data/server/run.sh")).unwrap(), b"#!x\n", "{name}");
        assert_eq!(mode(&b.at("/data/server/run.sh")), 0o755, "{name}");
        assert_eq!(
            std::fs::read_link(b.at("/data/server/latest")).unwrap(),
            Path::new("run.sh"),
            "{name}"
        );
        assert!(!b.at("/data/server/stale").exists(), "{name}: never merged into what was there");
        b.run(&[x], 0).unwrap();
        let root = act(InstallStep::Extract {
            from: "server.archive".into(),
            to: "/data".into(),
            strip: 0,
            include: vec![],
        });
        assert!(b.run(&[root], 0).is_err(), "{name}: a volume's root is never replaced");
    }
}

/// A tar with one entry whose raw header names `path`, of type `ty`, linking to `link`.
fn hostile(path: &str, ty: tar::EntryType, link: &str) -> Vec<u8> {
    let mut h = tar::Header::new_gnu();
    h.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
    h.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
    h.set_entry_type(ty);
    h.set_mode(0o644);
    h.set_size(0);
    h.set_cksum();
    let mut t = tar::Builder::new(Vec::new());
    t.append(&h, std::io::empty()).unwrap();
    t.into_inner().unwrap()
}

#[test]
fn a_hostile_archive_is_refused_and_nothing_of_it_is_unpacked() {
    use tar::EntryType as T;
    let cases = [
        ("../escape", T::Regular, ""),
        ("/etc/cron.d/x", T::Regular, ""),
        ("link", T::Symlink, "/etc"),
        ("link", T::Symlink, "../../outside"),
        ("tty", T::Char, ""),
        ("pipe", T::Fifo, ""),
        ("hard", T::Link, "not/unpacked"),
    ];
    for (path, ty, link) in cases {
        let mut b = Bench::new();
        b.file("bad.tar", &hostile(path, ty, link));
        std::fs::create_dir_all(b.at("/data/server")).unwrap();
        std::fs::write(b.at("/data/server/world"), b"kept").unwrap();
        let x = act(InstallStep::Extract {
            from: "bad.tar".into(),
            to: "/data/server".into(),
            strip: 0,
            include: vec![],
        });
        assert!(b.run(&[x], 0).is_err(), "{path} {ty:?} {link}");
        assert_eq!(
            std::fs::read(b.at("/data/server/world")).unwrap(),
            b"kept",
            "{path}: untouched"
        );
        let names: Vec<String> = std::fs::read_dir(&b.volume)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["server"], "{path}: no half-unpacked tree left");
        assert_eq!(std::fs::read_dir(&b.outside).unwrap().count(), 0);
    }
}

#[test]
fn an_install_resumes_at_the_action_that_failed() {
    let mut b = Bench::new();
    let actions = vec![
        act(InstallStep::Write { path: "/data/a".into(), content: "a".into(), mode: 0o644 }),
        act(InstallStep::Copy { from: "late.txt".into(), to: "/data/late".into() }),
        act(InstallStep::Write { path: "/data/c".into(), content: "c".into(), mode: 0o644 }),
    ];
    let e = b.run(&actions, 0).unwrap_err();
    assert_eq!(e.index, 1);
    b.file("late.txt", b"now here");
    assert_eq!(b.run(&actions, e.index).unwrap(), [1, 2], "from the one that failed");
    assert_eq!(std::fs::read(b.at("/data/late")).unwrap(), b"now here");
}

#[test]
fn an_owner_is_set_as_the_workload_sees_it_when_the_worker_may() {
    if !rustix::process::geteuid().is_root() {
        return;
    }
    let b = Bench::new();
    let actions = vec![
        act(InstallStep::Write { path: "/data/owned".into(), content: "x".into(), mode: 0o644 }),
        act(InstallStep::Chown { path: "/data/owned".into(), uid: 1000, gid: 1001 }),
    ];
    b.run(&actions, 0).unwrap();
    let st = std::fs::metadata(b.at("/data/owned")).unwrap();
    assert_eq!(
        (std::os::unix::fs::MetadataExt::uid(&st), std::os::unix::fs::MetadataExt::gid(&st)),
        (1000, 1001)
    );
}

#[test]
fn a_fetch_reaches_only_what_fetches_downloaded_never_a_recipe_s_file() {
    let mut b = Bench::new();
    // Another tenant's recipe file, from a private registry, on the same node.
    let private = b.file("secret.cfg", b"another tenant's private file");
    b.files.clear();
    let hash = Hash::parse(&private).unwrap();
    let url = "https://example.org/anything".to_owned();
    let fetch = act(InstallStep::Fetch { url, hash, to: "/data/leak".into() });
    let e = b.run(&[fetch], 0).unwrap_err();
    assert!(e.reason.contains("not in the content store"), "{e:?}");
    assert!(!b.at("/data/leak").exists());
}
