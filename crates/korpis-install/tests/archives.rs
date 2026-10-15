// SPDX-License-Identifier: AGPL-3.0-or-later

//! Archives an `extract` unpacks: each format replacing its target whole, hostile archives refused
//! with nothing of them unpacked, and a provider's archive unpacked from what fetches downloaded.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod bench;

use bench::{Bench, act, mode};
use korpis_core::recipe::install::InstallStep;
use std::io::Write as _;
use std::path::Path;

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
fn an_archive_a_provider_answered_is_unpacked_from_what_fetches_downloaded_only() {
    let mut b = Bench::new();
    let fetched = b.keep(&tar());
    let hex = fetched.strip_prefix("sha256:").unwrap();
    let from = format!("{}sha256/{hex}", korpis_core::recipe::install::FETCHED);
    let x =
        act(InstallStep::Extract { from, to: "/data/server".into(), strip: 1, include: vec![] });
    b.run(&[x], 0).unwrap();
    assert_eq!(std::fs::read(b.at("/data/server/run.sh")).unwrap(), b"#!x\n");
    // A recipe's file is not there, whatever its digest.
    let private = b.file("pkg.tar", &tar());
    let mut other = Bench::new();
    other.file("pkg.tar", &tar());
    let hex = private.strip_prefix("sha256:").unwrap();
    let from = format!("{}sha256/{hex}", korpis_core::recipe::install::FETCHED);
    let x = act(InstallStep::Extract { from, to: "/data/other".into(), strip: 1, include: vec![] });
    assert!(other.run(&[x], 0).is_err(), "only what a fetch downloaded");
}
