// SPDX-License-Identifier: AGPL-3.0-or-later

//! The example recipe and a registry to push it to.

use korpis_recipe::artifact::{Artifact, File};
use korpis_recipe::registry::Registry;
use uuid::Uuid;

pub const PAPER: &str = include_str!("paper.yaml");

/// The registry tests push to: `KORPIS_TEST_REGISTRY`, by default the `scripts/dev-registry.sh`
/// instance. It speaks plain HTTP.
pub fn address() -> String {
    std::env::var("KORPIS_TEST_REGISTRY").unwrap_or_else(|_| "127.0.0.1:55000".to_owned())
}

pub fn registry() -> Registry {
    Registry::new(vec![address()])
}

/// A repository no other test uses.
pub fn repository() -> String {
    format!("{}/test/paper-{}", address(), Uuid::now_v7().simple())
}

/// The example as an artifact carrying the files it refers to.
pub fn artifact(document: &str) -> Artifact {
    let file = |path: &str, data: &[u8]| File {
        path: path.to_owned(),
        media_type: "application/octet-stream".to_owned(),
        data: data.to_vec(),
    };
    Artifact {
        document: document.as_bytes().to_vec(),
        files: vec![
            file("layers/rootfs.tar.zst", b"not really a rootfs"),
            file("templates/server.properties", b"motd={{motd}}\n"),
        ],
    }
}
