// SPDX-License-Identifier: AGPL-3.0-or-later

//! OCI images as a node holds them (§6 of docs/04-runtimes.md): content addressed by digest,
//! fetched once by the agent and referenced by drivers. This is the part of the image format both
//! need and neither may get wrong: which manifest of an index is this machine's, what an image
//! manifest names, and the chain identifiers its unpacked layers are known by. Pure: bytes in,
//! decisions out, and every digest checked by the caller against the bytes it names.

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;

/// An OCI image index.
pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
/// An OCI image manifest.
pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
/// A Docker manifest list, the index's predecessor.
pub const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
/// A Docker image manifest, the manifest's predecessor.
pub const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
/// The largest manifest or index read, in bytes. Real ones are a few kilobytes.
pub const MAX_MANIFEST: usize = 4 << 20;

/// Why content is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContentError {
    /// Not a `sha256:` digest of 64 lowercase hexadecimal digits.
    #[error("{0:?} is not a sha256 digest")]
    Digest(String),
    /// Not a manifest or index this node reads.
    #[error("{0}")]
    Format(String),
    /// An index with no manifest for this machine.
    #[error("no manifest for {0}")]
    Platform(String),
}

/// A descriptor: what a manifest says of the content it names.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    /// Its media type.
    #[serde(default)]
    pub media_type: String,
    /// Its digest.
    pub digest: String,
    /// Its size in bytes.
    pub size: u64,
    /// For an index entry, the platform it is for.
    #[serde(default)]
    pub platform: Option<Platform>,
}

/// A platform, as an index names it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Platform {
    /// e.g. `linux`.
    pub os: String,
    /// e.g. `amd64`.
    pub architecture: String,
    /// e.g. `v8`.
    #[serde(default)]
    pub variant: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    #[serde(default)]
    media_type: String,
    #[serde(default)]
    manifests: Option<Vec<Descriptor>>,
    #[serde(default)]
    config: Option<Descriptor>,
    #[serde(default)]
    layers: Option<Vec<Descriptor>>,
}

/// What a manifest or index is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Document {
    /// An index: a manifest per platform.
    Index(Vec<Descriptor>),
    /// An image manifest: its configuration and its layers, bottom first.
    Image {
        /// The image configuration.
        config: Descriptor,
        /// The layers.
        layers: Vec<Descriptor>,
    },
}

/// The digest of `bytes`, as `sha256:` and lowercase hexadecimal.
#[must_use]
pub fn digest_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::from("sha256:"), |mut s, b| {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The hexadecimal part of a well-formed `sha256:` digest: what names a file in a store, and
/// so what must never be anything else.
pub fn hex_of(digest: &str) -> Result<&str, ContentError> {
    let hex = digest.strip_prefix("sha256:").ok_or_else(|| ContentError::Digest(digest.into()))?;
    let ok =
        hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok { Ok(hex) } else { Err(ContentError::Digest(digest.into())) }
}

/// Reads a manifest or an index.
pub fn read(bytes: &[u8]) -> Result<Document, ContentError> {
    if bytes.len() > MAX_MANIFEST {
        return Err(ContentError::Format(format!("larger than {MAX_MANIFEST} bytes")));
    }
    let e: Envelope =
        serde_json::from_slice(bytes).map_err(|e| ContentError::Format(e.to_string()))?;
    let check = |d: &Descriptor| hex_of(&d.digest).map(|_| ());
    match (e.manifests, e.config, e.layers) {
        (Some(ms), None, None)
            if e.media_type.is_empty()
                || e.media_type == OCI_INDEX
                || e.media_type == DOCKER_LIST =>
        {
            ms.iter().try_for_each(check)?;
            Ok(Document::Index(ms))
        }
        (None, Some(config), Some(layers)) => {
            check(&config)?;
            layers.iter().try_for_each(check)?;
            Ok(Document::Image { config, layers })
        }
        _ => Err(ContentError::Format(format!(
            "not an image manifest or index ({:?})",
            e.media_type
        ))),
    }
}

/// This machine's platform, as an index names it.
#[must_use]
pub fn this_machine() -> (&'static str, &'static str) {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        "powerpc64" => "ppc64le",
        other => other,
    };
    ("linux", arch)
}

/// The manifest of `index` for `os` and `architecture`.
pub fn select<'a>(
    index: &'a [Descriptor],
    (os, architecture): (&str, &str),
) -> Result<&'a Descriptor, ContentError> {
    index
        .iter()
        .find(|d| d.platform.as_ref().is_some_and(|p| p.os == os && p.architecture == architecture))
        .ok_or_else(|| ContentError::Platform(format!("{os}/{architecture}")))
}

/// The chain identifiers of layers with uncompressed digests `diff_ids`, bottom first: what each
/// unpacked prefix of an image is known by, so two images sharing a base share its snapshots.
#[must_use]
pub fn chain_ids(diff_ids: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(diff_ids.len());
    for d in diff_ids {
        let next = match out.last() {
            None => d.clone(),
            Some(parent) => digest_of(format!("{parent} {d}").as_bytes()),
        };
        out.push(next);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: char) -> String {
        format!("sha256:{}", n.to_string().repeat(64))
    }

    #[test]
    fn an_index_yields_this_machines_manifest_and_a_manifest_its_layers() {
        let index = format!(
            r#"{{"schemaVersion":2,"mediaType":"{OCI_INDEX}","manifests":[
              {{"digest":"{}","size":1,"platform":{{"os":"linux","architecture":"arm64"}}}},
              {{"digest":"{}","size":1,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#,
            d('a'),
            d('b')
        );
        let Document::Index(ms) = read(index.as_bytes()).unwrap() else { panic!("an index") };
        assert_eq!(select(&ms, ("linux", "amd64")).unwrap().digest, d('b'));
        assert!(select(&ms, ("linux", "s390x")).is_err());
        let image = format!(
            r#"{{"schemaVersion":2,"config":{{"digest":"{}","size":2}},
               "layers":[{{"digest":"{}","size":3}}]}}"#,
            d('c'),
            d('e')
        );
        let Document::Image { config, layers } = read(image.as_bytes()).unwrap() else {
            panic!("an image")
        };
        assert_eq!((config.digest, layers.len()), (d('c'), 1));
        let bad = image.replace(&d('e'), "sha256:../../etc/passwd");
        assert!(read(bad.as_bytes()).is_err(), "a digest names a file, so it is checked");
    }

    #[test]
    fn chain_ids_follow_the_image_spec() {
        let ids = chain_ids(&[d('1'), d('2')]);
        assert_eq!(ids[0], d('1'));
        assert_eq!(ids[1], digest_of(format!("{} {}", d('1'), d('2')).as_bytes()));
        assert!(hex_of("sha256:ABC").is_err());
    }
}
