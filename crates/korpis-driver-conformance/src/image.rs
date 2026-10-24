// SPDX-License-Identifier: Apache-2.0

//! The probe's image, laid into the node's content store as the agent lays an image it fetched
//! (§6 of docs/04-runtimes.md): each blob at `blobs/sha256/<hex>`, written whole under another
//! name, synced, and renamed, so the store never holds part of a blob. The image is an OCI image
//! of one uncompressed layer holding the probe, at `/probe`, and the directories a root
//! filesystem is expected to have; its reference names it by its manifest's digest, as a recipe's
//! does.

use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::Path;

/// The name the probe's image is referred to by.
pub const NAME: &str = "korpis.dev/conformance/probe";

/// Where the probe is in its image.
pub const PROBE: &str = "/probe";

/// The directories of the image's root, and each one's mode.
const DIRECTORIES: [(&str, u32); 7] = [
    ("dev/", 0o755),
    ("etc/", 0o755),
    ("proc/", 0o755),
    ("run/", 0o755),
    ("sys/", 0o755),
    ("tmp/", 0o1777),
    ("var/", 0o755),
];

/// The size of a tar block.
const BLOCK: usize = 512;

/// This machine's architecture, as an image and a recipe name it.
#[must_use]
pub fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Writes `bytes` into `header` at `at`.
fn put(header: &mut [u8; BLOCK], at: usize, bytes: &[u8]) {
    for (to, from) in header.iter_mut().skip(at).zip(bytes) {
        *to = *from;
    }
}

/// Writes `value` into the `width`-byte numeric field at `at`: octal, zero-padded, and ended.
fn octal(header: &mut [u8; BLOCK], (at, width): (usize, usize), value: u64) {
    let digits = format!("{value:0w$o}", w = width.saturating_sub(1));
    put(header, at, digits.as_bytes());
    put(header, at + width - 1, &[0]);
}

/// A ustar header for `name`, a directory when `kind` is `b'5'` and a file of `size` bytes when
/// it is `b'0'`, owned by root with `mode`.
fn header(name: &str, mode: u32, size: u64, kind: u8) -> [u8; BLOCK] {
    let mut h = [0u8; BLOCK];
    put(&mut h, 0, name.as_bytes());
    octal(&mut h, (100, 8), u64::from(mode));
    octal(&mut h, (108, 8), 0);
    octal(&mut h, (116, 8), 0);
    octal(&mut h, (124, 12), size);
    octal(&mut h, (136, 12), 0);
    put(&mut h, 156, &[kind]);
    put(&mut h, 257, b"ustar\x0000");
    // The checksum is taken with its own field as spaces.
    put(&mut h, 148, b"        ");
    let sum: u64 = h.iter().map(|b| u64::from(*b)).sum();
    let digits = format!("{sum:06o}");
    put(&mut h, 148, digits.as_bytes());
    put(&mut h, 154, b"\0 ");
    h
}

/// The layer: the root's directories and the probe, as an uncompressed tar.
fn layer(probe: &[u8]) -> Vec<u8> {
    let mut tar = Vec::with_capacity(probe.len() + 16 * BLOCK);
    for (dir, mode) in DIRECTORIES {
        tar.extend_from_slice(&header(dir, mode, 0, b'5'));
    }
    let size = u64::try_from(probe.len()).unwrap_or(u64::MAX);
    tar.extend_from_slice(&header(PROBE.trim_start_matches('/'), 0o755, size, b'0'));
    tar.extend_from_slice(probe);
    let padding = (BLOCK - probe.len() % BLOCK) % BLOCK;
    tar.resize(tar.len() + padding + 2 * BLOCK, 0);
    tar
}

/// `bytes`'s digest, `sha256:<hex>`.
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::from("sha256:"), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Lays `bytes` into the store at `content`; its digest and size.
fn blob(content: &Path, bytes: &[u8]) -> io::Result<(String, u64)> {
    let digest = digest(bytes);
    let dir = content.join("blobs/sha256");
    std::fs::create_dir_all(&dir)?;
    let hex = digest.trim_start_matches("sha256:");
    let path = dir.join(hex);
    if !path.exists() {
        let ingest = dir.join(format!(".ingest-{hex}"));
        let mut file = std::fs::File::create(&ingest)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&ingest, &path)?;
    }
    Ok((digest, u64::try_from(bytes.len()).unwrap_or(u64::MAX)))
}

/// Lays the image of the probe at `probe` into the store at `content`; its reference, by the
/// manifest's digest.
///
/// # Errors
/// The probe could not be read, or the store written.
pub fn lay(content: &Path, probe: &Path) -> io::Result<String> {
    let (layer, layer_size) = blob(content, &layer(&std::fs::read(probe)?))?;
    let config = serde_json::json!({
        "architecture": arch(),
        "os": "linux",
        "config": {},
        "rootfs": {"type": "layers", "diff_ids": [layer]},
    });
    let (config, config_size) = blob(content, config.to_string().as_bytes())?;
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config,
            "size": config_size,
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar",
            "digest": layer,
            "size": layer_size,
        }],
    });
    let (manifest, _) = blob(content, manifest.to_string().as_bytes())?;
    Ok(format!("{NAME}@{manifest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_s_checksum_counts_its_own_field_as_spaces() {
        let h = header("probe", 0o755, 3, b'0');
        let mut blanked = h;
        put(&mut blanked, 148, b"        ");
        let sum: u64 = blanked.iter().map(|b| u64::from(*b)).sum();
        let field = String::from_utf8(h.get(148..154).unwrap_or_default().to_vec()).unwrap();
        assert_eq!(u64::from_str_radix(&field, 8).unwrap(), sum);
        assert_eq!(h.get(257..265), Some(&b"ustar\x0000"[..]));
    }

    #[test]
    fn the_layer_is_whole_blocks_ending_in_two_empty_ones() {
        let tar = layer(b"#!probe");
        assert_eq!(tar.len() % BLOCK, 0);
        assert!(tar.iter().rev().take(2 * BLOCK).all(|b| *b == 0));
    }
}
