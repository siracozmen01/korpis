// SPDX-License-Identifier: AGPL-3.0-or-later

//! An OCI image made in a content store, for tests: its layers written from a few kinds of entry,
//! packed as each test asks, and named by its manifest's digest as an intent names an image.

use crate::source::Packing;
use korpis_core::content::digest_of;
use std::io::{self, Write as _};
use std::path::Path;

/// One entry of a layer.
#[derive(Debug, Clone, Copy)]
pub enum Entry<'a> {
    /// A file with its contents, owner, and mode.
    File(&'a str, &'a [u8], (u32, u32), u32),
    /// A symlink to the target given.
    Symlink(&'a str, &'a str),
    /// A whiteout, removing the path given from the layers below.
    Whiteout(&'a str),
}

fn header(kind: tar::EntryType, path: &str, (uid, gid): (u32, u32), mode: u32) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(kind);
    h.set_mode(mode);
    h.set_uid(u64::from(uid));
    h.set_gid(u64::from(gid));
    h.set_mtime(1_700_000_000);
    h.set_size(0);
    let _ = h.set_path(path);
    h
}

/// A layer holding `entries`, as a bare tar.
pub fn layer(entries: &[Entry<'_>]) -> io::Result<Vec<u8>> {
    let mut b = tar::Builder::new(Vec::new());
    for e in entries {
        match *e {
            Entry::File(path, data, owner, mode) => {
                let mut h = header(tar::EntryType::Regular, path, owner, mode);
                h.set_size(u64::try_from(data.len()).unwrap_or(u64::MAX));
                h.set_cksum();
                b.append(&h, data)?;
            }
            Entry::Symlink(path, target) => {
                let mut h = header(tar::EntryType::Symlink, path, (0, 0), 0o777);
                b.append_link(&mut h, path, target)?;
            }
            Entry::Whiteout(path) => {
                let p = Path::new(path);
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
                let marked = p.with_file_name(format!(".wh.{}", name.unwrap_or_default()));
                let mut h = header(tar::EntryType::Regular, &marked.to_string_lossy(), (0, 0), 0);
                h.set_size(0);
                h.set_cksum();
                b.append(&h, io::empty())?;
            }
        }
    }
    b.into_inner()
}

fn put(content: &Path, bytes: &[u8]) -> io::Result<String> {
    let digest = digest_of(bytes);
    let hex = digest.trim_start_matches("sha256:");
    std::fs::create_dir_all(content.join("blobs/sha256"))?;
    std::fs::write(content.join("blobs/sha256").join(hex), bytes)?;
    Ok(digest)
}

/// `tar` packed as `packing`, and the media type that says so.
fn packed(tar: &[u8], packing: Packing) -> io::Result<(Vec<u8>, String)> {
    let oci = "application/vnd.oci.image.layer.v1.tar";
    Ok(match packing {
        Packing::Tar => (tar.to_vec(), oci.to_owned()),
        Packing::Gzip => {
            let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            z.write_all(tar)?;
            (z.finish()?, format!("{oci}+gzip"))
        }
        Packing::Zstd => (zstd::encode_all(tar, 3)?, format!("{oci}+zstd")),
    })
}

/// An image of `layers`, bottom first, each a bare tar packed as given, written into the content
/// store at `content`, whose configuration runs as `user`: `name@sha256:...`.
pub fn image(content: &Path, layers: &[(Vec<u8>, Packing)], user: &str) -> io::Result<String> {
    let mut diff_ids = Vec::new();
    let mut described = Vec::new();
    for (tar, packing) in layers {
        diff_ids.push(digest_of(tar));
        let (bytes, media) = packed(tar, *packing)?;
        let digest = put(content, &bytes)?;
        described.push(serde_json::json!({"mediaType": media, "digest": digest,
                                          "size": bytes.len()}));
    }
    let config = serde_json::json!({
        "architecture": korpis_core::content::this_machine().1, "os": "linux",
        "config": {"Env": ["PATH=/bin"], "User": user},
        "rootfs": {"type": "layers", "diff_ids": diff_ids},
    });
    let config = serde_json::to_vec(&config).map_err(io::Error::other)?;
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": korpis_core::content::OCI_MANIFEST,
        "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                   "digest": put(content, &config)?, "size": config.len()},
        "layers": described,
    });
    let manifest = serde_json::to_vec(&manifest).map_err(io::Error::other)?;
    Ok(format!("test/image@{}", put(content, &manifest)?))
}
