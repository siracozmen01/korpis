// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an image's configuration says about running it: its default command, environment,
//! working directory, and user, and the uncompressed digests of its layers. An image is
//! untrusted input, so what it says is bounded and checked like anything else from outside.
//! Every driver that runs an image reads it here, so a `container` and a `microvm` run one image
//! the same way (§6 of docs/04-runtimes.md).

use serde::Deserialize;

/// The largest image configuration read, in bytes.
pub const MAX_CONFIG: usize = 1 << 20;

/// Why an image configuration is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("image configuration: {0}")]
pub struct ImageError(pub String);

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Run {
    #[serde(default)]
    user: String,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    entrypoint: Option<Vec<String>>,
    #[serde(default)]
    cmd: Option<Vec<String>>,
    #[serde(default)]
    working_dir: String,
}

#[derive(Debug, Deserialize)]
struct RootFs {
    #[serde(default)]
    diff_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Raw {
    #[serde(default)]
    config: Option<Run>,
    rootfs: RootFs,
}

/// An image's configuration, as far as running it goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageConfig {
    /// The user and group the workload runs as, inside its user namespace.
    pub user: (u32, u32),
    /// `KEY=value` entries.
    pub env: Vec<String>,
    /// The entrypoint.
    pub entrypoint: Vec<String>,
    /// The default arguments.
    pub cmd: Vec<String>,
    /// The working directory; empty for `/`.
    pub working_dir: String,
    /// The uncompressed digests of the layers, bottom first.
    pub diff_ids: Vec<String>,
}

/// `uid[:gid]`, numeric. A name would have to be looked up in the image's own `/etc/passwd`,
/// which is not read; such an image is refused rather than run as someone else.
fn user(spec: &str) -> Result<(u32, u32), ImageError> {
    if spec.is_empty() {
        return Ok((0, 0));
    }
    let (u, g) = spec.split_once(':').unwrap_or((spec, ""));
    let uid: u32 = u
        .parse()
        .map_err(|_| ImageError(format!("user {spec:?}: only numeric users are run, not names")))?;
    let gid = if g.is_empty() {
        uid
    } else {
        g.parse().map_err(|_| ImageError(format!("group in {spec:?}: only numeric groups")))?
    };
    if uid > 65535 || gid > 65535 {
        return Err(ImageError(format!("user {spec:?} is outside the workload's 65536 ids")));
    }
    Ok((uid, gid))
}

impl ImageConfig {
    /// Reads an image configuration.
    pub fn read(bytes: &[u8]) -> Result<Self, ImageError> {
        if bytes.len() > MAX_CONFIG {
            return Err(ImageError(format!("larger than {MAX_CONFIG} bytes")));
        }
        let raw: Raw = serde_json::from_slice(bytes).map_err(|e| ImageError(e.to_string()))?;
        for d in &raw.rootfs.diff_ids {
            super::hex_of(d).map_err(|e| ImageError(e.to_string()))?;
        }
        let run = raw.config.unwrap_or_default();
        Ok(Self {
            user: user(&run.user)?,
            env: run.env,
            entrypoint: run.entrypoint.unwrap_or_default(),
            cmd: run.cmd.unwrap_or_default(),
            working_dir: run.working_dir,
            diff_ids: raw.rootfs.diff_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configuration_is_read_and_a_named_user_refused() {
        let diff = format!("sha256:{}", "b".repeat(64));
        let json = format!(
            r#"{{"config":{{"User":"1000:100","Env":["PATH=/bin"],"Cmd":["sh"],
                 "WorkingDir":"/srv"}},"rootfs":{{"type":"layers","diff_ids":["{diff}"]}}}}"#
        );
        let c = ImageConfig::read(json.as_bytes()).unwrap();
        assert_eq!(
            (c.user, c.cmd.clone(), c.working_dir.as_str()),
            ((1000, 100), vec!["sh".into()], "/srv")
        );
        assert_eq!(c.diff_ids, vec![diff.clone()]);
        let named = json.replace("1000:100", "nobody");
        assert!(ImageConfig::read(named.as_bytes()).is_err());
        let outside = json.replace("1000:100", "70000");
        assert!(ImageConfig::read(outside.as_bytes()).is_err());
        let bad = json.replace(&diff, "sha256:..");
        assert!(ImageConfig::read(bad.as_bytes()).is_err());
    }
}
