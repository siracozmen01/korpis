// SPDX-License-Identifier: AGPL-3.0-or-later

//! An image drive built by `korpis-image-build` from a content store (§6 of
//! docs/04-runtimes.md): two layers, packed two ways, the upper whiting out a file of the lower;
//! the drive is root's and read-only, its files owned by the workload's own ids; and a layer
//! that is not its digest, or an owner beyond the workload's ids, leaves no drive and no work
//! behind. Needs root and `debugfs`, and is skipped without root.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

use korpis_image::build::{Request, file_name, run};
use korpis_image::source::Packing;
use korpis_image::testing::{Entry, image, layer};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A range of ids above any a host's users take, which no workload of this test process has.
const IDS: u32 = 1_900_000_000;

fn root() -> bool {
    rustix::process::geteuid().is_root()
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_korpis-image-build"))
}

/// What `debugfs` says when asked `request` of the filesystem in `drive`.
fn debugfs(drive: &Path, request: &str) -> String {
    let out = Command::new("debugfs").args(["-R", request]).arg(drive).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn two_layers(content: &Path) -> String {
    let lower = layer(&[
        Entry::File("etc/kept", b"kept", (1000, 1000), 0o640),
        Entry::File("etc/gone", b"gone", (0, 0), 0o644),
        Entry::Symlink("bin/sh", "busybox"),
    ])
    .unwrap();
    let upper =
        layer(&[Entry::Whiteout("etc/gone"), Entry::File("etc/added", b"a", (0, 0), 0o644)])
            .unwrap();
    image(content, &[(lower, Packing::Gzip), (upper, Packing::Zstd)], "1000:1000").unwrap()
}

fn request(content: &Path, out: &Path, image: &str) -> Request {
    let source = korpis_image::source::resolve(content, image).unwrap();
    Request {
        content: content.to_owned(),
        image: image.to_owned(),
        out: out.join(file_name(&source.manifest)),
        ids: IDS,
    }
}

fn left_behind(out: &Path) -> Vec<String> {
    let names = std::fs::read_dir(out).unwrap().map(|e| e.unwrap().file_name());
    names.map(|n| n.to_string_lossy().into_owned()).collect()
}

#[test]
fn a_drive_holds_the_layers_as_the_image_sees_them_owned_by_the_workload_s_ids() {
    if !root() {
        return;
    }
    let (content, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let image = two_layers(content.path());
    let asked = request(content.path(), out.path(), &image);
    run(&binary(), &asked).unwrap();
    let drive = std::fs::metadata(&asked.out).unwrap();
    assert_eq!((drive.uid(), drive.gid(), drive.mode() & 0o777), (0, 0, 0o444));
    let etc = debugfs(&asked.out, "ls -l /etc");
    assert!(etc.contains("kept") && etc.contains("added") && !etc.contains("gone"), "{etc}");
    let kept = debugfs(&asked.out, "stat /etc/kept");
    assert!(kept.contains("User:  1000") && kept.contains("Group:  1000"), "{kept}");
    let named = asked.out.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(left_behind(out.path()), [named], "the drive, and no work directory");
}

#[test]
fn a_layer_not_its_digest_or_owned_beyond_the_ids_leaves_no_drive() {
    if !root() {
        return;
    }
    let (content, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let image = two_layers(content.path());
    let asked = request(content.path(), out.path(), &image);
    let source = korpis_image::source::resolve(content.path(), &image).unwrap();
    let blob = korpis_image::source::blob(content.path(), &source.layers[1].0.digest).unwrap();
    let mut bytes = std::fs::read(&blob).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&blob, bytes).unwrap();
    assert!(run(&binary(), &asked).is_err(), "a layer that is not its digest");
    assert!(left_behind(out.path()).is_empty(), "no drive and no work directory");

    let foreign = layer(&[Entry::File("x", b"x", (70_000, 0), 0o644)]).unwrap();
    let image =
        korpis_image::testing::image(content.path(), &[(foreign, Packing::Tar)], "0").unwrap();
    let asked = request(content.path(), out.path(), &image);
    assert!(run(&binary(), &asked).is_err(), "an owner beyond the workload's ids");
    assert!(left_behind(out.path()).is_empty());
}
