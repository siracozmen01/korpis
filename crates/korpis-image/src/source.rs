// SPDX-License-Identifier: AGPL-3.0-or-later

//! An image as the node's content store holds it (§6 of docs/04-runtimes.md): the manifest for
//! this machine, chosen from an index where the image is one, its configuration, and its layers,
//! each read from `blobs/sha256` and checked against its digest. The store is the agent's to
//! fill; this only reads it.

use korpis_core::content::image::ImageConfig;
use korpis_core::content::{self as oci, Descriptor, Document};
use std::io;
use std::path::{Path, PathBuf};

/// How a layer's bytes are packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packing {
    /// A bare tar.
    Tar,
    /// A tar in gzip.
    Gzip,
    /// A tar in zstd.
    Zstd,
}

/// An image, resolved for this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The digest of the manifest chosen: what the image's drive is named by.
    pub manifest: String,
    /// What the image says about running it.
    pub config: ImageConfig,
    /// Its layers, bottom first.
    pub layers: Vec<(Descriptor, Packing)>,
}

fn invalid(why: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

/// The packing a layer's media type names, refused when it is none of the three.
pub fn packing(media_type: &str) -> io::Result<Packing> {
    // Media types are compared exactly, as the specifications write them: lower case.
    match media_type.rsplit_once(['+', '.']) {
        Some((_, "gzip")) => Ok(Packing::Gzip),
        Some((_, "zstd")) => Ok(Packing::Zstd),
        Some((_, "tar")) => Ok(Packing::Tar),
        _ => Err(invalid(format!("a layer of media type {media_type:?}"))),
    }
}

/// Where the blob `digest` is in the store at `content`.
pub fn blob(content: &Path, digest: &str) -> io::Result<PathBuf> {
    Ok(content.join("blobs/sha256").join(oci::hex_of(digest).map_err(invalid)?))
}

/// The blob `digest`, read whole and checked; for documents, which are small.
fn document(content: &Path, digest: &str) -> io::Result<Vec<u8>> {
    let path = blob(content, digest)?;
    let bytes = std::fs::read(&path)
        .map_err(|e| invalid(format!("{digest} is not in the node's content store: {e}")))?;
    if bytes.len() > oci::MAX_MANIFEST.max(korpis_core::content::image::MAX_CONFIG) {
        return Err(invalid(format!("{digest} is larger than any document")));
    }
    if oci::digest_of(&bytes) != digest {
        return Err(invalid(format!("the content store's {digest} does not match its digest")));
    }
    Ok(bytes)
}

/// The image `rootfs`, `name@sha256:...`, as the store at `content` holds it.
pub fn resolve(content: &Path, rootfs: &str) -> io::Result<Source> {
    let (_, top) = rootfs.split_once('@').ok_or_else(|| invalid("an image by its digest"))?;
    let mut manifest = top.to_owned();
    let mut doc = oci::read(&document(content, top)?).map_err(invalid)?;
    if let Document::Index(ms) = &doc {
        manifest.clone_from(&oci::select(ms, oci::this_machine()).map_err(invalid)?.digest);
        doc = oci::read(&document(content, &manifest)?).map_err(invalid)?;
    }
    let Document::Image { config, layers } = doc else {
        return Err(invalid("an index names an index"));
    };
    let config = ImageConfig::read(&document(content, &config.digest)?).map_err(invalid)?;
    if config.diff_ids.len() != layers.len() {
        return Err(invalid("the configuration and the manifest disagree on the layers"));
    }
    let layers = layers
        .into_iter()
        .map(|l| packing(&l.media_type).map(|p| (l, p)))
        .collect::<io::Result<_>>()?;
    Ok(Source { manifest, config, layers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_packing_is_read_from_its_media_type_and_others_refused() {
        let oci = "application/vnd.oci.image.layer.v1.tar";
        assert_eq!(packing(&format!("{oci}+gzip")).unwrap(), Packing::Gzip);
        assert_eq!(packing(&format!("{oci}+zstd")).unwrap(), Packing::Zstd);
        assert_eq!(packing(oci).unwrap(), Packing::Tar);
        let docker = "application/vnd.docker.image.rootfs.diff.tar.gzip";
        assert_eq!(packing(docker).unwrap(), Packing::Gzip);
        assert!(packing("application/vnd.oci.image.layer.nondistributable.v1.tar+foo").is_err());
    }
}
