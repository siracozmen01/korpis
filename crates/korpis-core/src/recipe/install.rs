// SPDX-License-Identifier: AGPL-3.0-or-later

//! The install DSL (§4 of docs/09-recipes.md). The verb list is closed and none of them runs
//! code; a step that needs more is a provider step, answered by an extension in its own sandbox.

use super::template::TemplateFormat;
use super::{RecipeError, artifact_path, workload_path};
use crate::value::ValueMap;

/// A content hash, mandatory on every fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hash {
    /// SHA-256, 64 lowercase hexadecimal digits.
    Sha256(String),
    /// SHA-512, 128 lowercase hexadecimal digits.
    Sha512(String),
}

impl Hash {
    /// Parses `sha256:<hex>` or `sha512:<hex>`.
    pub fn parse(s: &str) -> Option<Self> {
        let hex = |h: &str, n: usize| {
            (h.len() == n && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                .then(|| h.to_owned())
        };
        if let Some(h) = s.strip_prefix("sha256:") {
            return hex(h, 64).map(Self::Sha256);
        }
        s.strip_prefix("sha512:").and_then(|h| hex(h, 128)).map(Self::Sha512)
    }

    /// The `algorithm:hex` form.
    #[must_use]
    pub fn to_digest(&self) -> String {
        match self {
            Self::Sha256(h) => format!("sha256:{h}"),
            Self::Sha512(h) => format!("sha512:{h}"),
        }
    }
}

/// One install step.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallStep {
    /// Download through the node's content store, verified against the hash.
    Fetch {
        /// `https://` or `http://`; the hash, not the transport, is what is trusted.
        url: String,
        /// The content's hash.
        hash: Hash,
        /// Where it goes in the workload.
        to: String,
    },
    /// Copy content from inside the recipe artifact.
    Copy {
        /// A path inside the artifact.
        from: String,
        /// Where it goes in the workload.
        to: String,
    },
    /// Unpack an archive; traversal and absolute entries are refused when it is unpacked.
    Extract {
        /// The archive, a path in the workload or the artifact.
        from: String,
        /// Where it is unpacked.
        to: String,
        /// Leading path components to drop.
        strip: u32,
        /// Only these entries, if any are named.
        include: Vec<String>,
    },
    /// Create a directory.
    Mkdir {
        /// The directory.
        path: String,
        /// Its mode.
        mode: u32,
    },
    /// Write literal content from the recipe.
    Write {
        /// The file.
        path: String,
        /// The content.
        content: String,
        /// Its mode.
        mode: u32,
    },
    /// Render a template (§6 of docs/09-recipes.md).
    Template {
        /// The template, inside the artifact.
        from: String,
        /// The output.
        to: String,
        /// How values are encoded in it.
        format: TemplateFormat,
        /// The allow-listed variables.
        vars: Vec<String>,
    },
    /// Set a mode.
    Chmod {
        /// The path.
        path: String,
        /// The mode.
        mode: u32,
    },
    /// Set an owner within the tenant's uid range.
    Chown {
        /// The path.
        path: String,
        /// The user, relative to the tenant's range.
        uid: u32,
        /// The group, relative to the tenant's range.
        gid: u32,
    },
    /// Create a symbolic link; both ends must resolve inside the volume.
    Symlink {
        /// What it points to.
        target: String,
        /// The link.
        link: String,
    },
    /// Delete a path.
    Delete {
        /// The path.
        path: String,
    },
    /// An extension-provided step, pinned in the lockfile.
    Provider {
        /// `<publisher>/<step>`.
        provider: String,
        /// The workload path, in one of its volumes, that the artifacts the provider answers are
        /// written beneath: their paths are relative to it.
        to: String,
        /// Its arguments, validated by the provider's schema; `to` is never one of them.
        args: ValueMap,
    },
}

/// Where an install worker finds what a `fetch` downloaded, by `<alg>/<hex>`: the `from` of an
/// `extract` the node builds to unpack a step provider's archive. No recipe names it, since no
/// recipe names a path beneath `/.korpis`.
pub const FETCHED: &str = "/.korpis/content/fetched/";

const MAX_MODE: u32 = 0o7777;
/// The highest id a workload sees (§4.5 of docs/04-runtimes.md).
const MAX_ID: u32 = 65_535;

fn mode(at: &str, m: u32) -> Result<(), RecipeError> {
    if m > MAX_MODE { Err(RecipeError::new(at, "a mode is at most 07777")) } else { Ok(()) }
}

fn provider_name(at: &str, p: &str) -> Result<(), RecipeError> {
    let part = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b))
    };
    match p.split_once('/') {
        Some((publisher, step)) if part(publisher) && part(step) => Ok(()),
        _ => Err(RecipeError::new(at, "a provider step is <publisher>/<step>")),
    }
}

impl InstallStep {
    /// Checks the step's own arguments. Template variables are checked against the schema by
    /// the recipe, which holds both.
    pub fn validate(&self, at: &str) -> Result<(), RecipeError> {
        let f = |name: &str| format!("{at}.{name}");
        match self {
            Self::Fetch { url, to, .. } => {
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(RecipeError::new(&f("url"), "an http or https URL"));
                }
                workload_path(&f("to"), to)
            }
            Self::Copy { from, to } | Self::Template { from, to, .. } => {
                artifact_path(&f("from"), from)?;
                workload_path(&f("to"), to)
            }
            Self::Extract { from, to, include, .. } => {
                if from.starts_with('/') {
                    workload_path(&f("from"), from)?;
                } else {
                    artifact_path(&f("from"), from)?;
                }
                if include.iter().any(|e| e.starts_with('/') || artifact_path("", e).is_err()) {
                    return Err(RecipeError::new(&f("include"), "archive paths without . or .."));
                }
                workload_path(&f("to"), to)
            }
            Self::Mkdir { path, mode: m }
            | Self::Write { path, mode: m, .. }
            | Self::Chmod { path, mode: m } => {
                mode(&f("mode"), *m)?;
                workload_path(&f("path"), path)
            }
            Self::Chown { path, uid, gid } => {
                // Workload-relative ids (§4.5 of docs/04-runtimes.md).
                if *uid > MAX_ID || *gid > MAX_ID {
                    return Err(RecipeError::new(at, "uid and gid are 0 to 65535"));
                }
                workload_path(&f("path"), path)
            }
            Self::Delete { path } => workload_path(&f("path"), path),
            Self::Symlink { target, link } => {
                workload_path(&f("link"), link)?;
                if target.starts_with('/') {
                    workload_path(&f("target"), target)
                } else {
                    artifact_path(&f("target"), target)
                }
            }
            Self::Provider { provider, to, .. } => {
                provider_name(&f("provider"), provider)?;
                workload_path(&f("to"), to)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Hash;

    #[test]
    fn hashes_name_their_algorithm() {
        let h = Hash::parse(&format!("sha512:{}", "0".repeat(128))).unwrap();
        assert!(matches!(h, Hash::Sha512(_)));
        assert!(Hash::parse(&format!("sha256:{}", "0".repeat(63))).is_none());
        assert!(Hash::parse(&format!("md5:{}", "0".repeat(32))).is_none());
    }
}
