// SPDX-License-Identifier: AGPL-3.0-or-later

//! A recipe step provider's answer (§4 of docs/09-recipes.md, §5 of docs/16-extensions.md): what
//! a step resolved to, as content-addressed artifacts the node's content store fetches and the
//! workload's own filesystem worker copies in, exactly as it does for a `fetch` verb. The provider
//! never writes a tenant's volume, so re-running a step re-resolves it and overwrites the same
//! targets, and its idempotence is the interface's, not a promise of each provider.

/// The most artifacts one step may produce: an application of many files is shipped as archives.
pub const MAX_ARTIFACTS: usize = 1000;

/// The longest path an artifact is written to, in bytes.
pub const MAX_PATH: usize = 4096;

/// The platform a step resolves for: the node's, since content may differ per platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    /// `linux`, `windows`.
    pub os: String,
    /// `amd64`, `arm64`.
    pub arch: String,
}

/// One artifact a step produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepArtifact {
    /// Where it goes, relative to the `to` of the recipe step that asked (§4 of
    /// docs/09-recipes.md).
    pub path: String,
    /// Where the content store fetches it from.
    pub url: String,
    /// Its SHA-256, lowercase hex: mandatory, as for every `fetch`.
    pub sha256: String,
    /// Its size in bytes.
    pub size: u64,
    /// Its file mode; ignored for an archive.
    pub mode: u32,
    /// An archive (tar, optionally gzip or zstd) unpacked at `path` under the `extract` verb's
    /// rules, rather than a file written there.
    pub extract: bool,
}

/// What a step resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    /// What to put where.
    pub artifacts: Vec<StepArtifact>,
    /// Whether the request named the content exactly (a depot manifest, say), so the same request
    /// always produces the same artifacts; an unpinned result marks the workload
    /// non-reproducible (§4 of docs/09-recipes.md).
    pub pinned: bool,
    /// What it resolved to, for the plan and the audit record, e.g. the manifest a channel held.
    pub resolved: String,
}

/// Whether `path` stays beneath where it is written: relative, no `..`, no empty or `.`
/// components.
fn beneath(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH
        && !path.starts_with('/')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

impl StepArtifact {
    /// Why it cannot be fetched and written as it says, if it cannot.
    #[must_use]
    pub fn problem(&self) -> Option<&'static str> {
        if !beneath(&self.path) {
            return Some("its path climbs out of where it is written");
        }
        if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
            return Some("its url is not http or https");
        }
        let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        if self.sha256.len() != 64 || !self.sha256.bytes().all(hex) {
            return Some("its sha256 is not 64 lowercase hex digits");
        }
        if self.mode > 0o7777 {
            return Some("its mode is not a file mode");
        }
        None
    }
}

impl StepResult {
    /// Why the node could not act on it, if it could not: a provider answering this is broken.
    #[must_use]
    pub fn problem(&self) -> Option<String> {
        if self.artifacts.len() > MAX_ARTIFACTS {
            return Some(format!("more than {MAX_ARTIFACTS} artifacts"));
        }
        self.artifacts
            .iter()
            .enumerate()
            .find_map(|(i, a)| a.problem().map(|why| format!("artifacts[{i}]: {why}")))
    }
}

/// Checks a recipe step's arguments against the schema its provider declares: every required
/// one present, none undeclared, each of its declared type. The field it names is the argument's.
pub fn check_arguments(
    schema: &[super::ActionArgument],
    arguments: &crate::value::ValueMap,
) -> Result<(), crate::recipe::RecipeError> {
    use super::ArgumentType;
    use crate::recipe::RecipeError;
    use crate::value::Value;
    if let Some(missing) = schema.iter().find(|a| a.required && !arguments.contains_key(&a.name)) {
        return Err(RecipeError::new(&missing.name, "required by the step"));
    }
    for (name, value) in arguments {
        let Some(declared) = schema.iter().find(|a| &a.name == name) else {
            return Err(RecipeError::new(name, "not an argument the step declares"));
        };
        let fits = matches!(
            (declared.kind, value),
            (ArgumentType::String, Value::String(_))
                | (ArgumentType::Int, Value::Int(_))
                | (ArgumentType::Bool, Value::Bool(_))
        );
        if !fits {
            return Err(RecipeError::new(name, format!("a {}", declared.kind.as_str())));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::{ActionArgument, ArgumentType};
    use crate::value::{Value, ValueMap};

    #[test]
    fn a_steps_arguments_are_judged_by_the_schema_its_provider_declares() {
        let schema = vec![
            ActionArgument { name: "appid".into(), kind: ArgumentType::Int, required: true },
            ActionArgument { name: "branch".into(), kind: ArgumentType::String, required: false },
        ];
        let args = |pairs: &[(&str, Value)]| -> ValueMap {
            pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
        };
        assert!(check_arguments(&schema, &args(&[("appid", Value::Int(730))])).is_ok());
        let field = |a: ValueMap| check_arguments(&schema, &a).unwrap_err().field;
        assert_eq!(field(args(&[])), "appid");
        assert_eq!(field(args(&[("appid", Value::String("730".into()))])), "appid");
        assert_eq!(field(args(&[("appid", Value::Int(1)), ("depot", Value::Int(2))])), "depot");
    }

    fn artifact(path: &str) -> StepArtifact {
        StepArtifact {
            path: path.into(),
            url: "https://cdn.example/a.tar.zst".into(),
            sha256: "a".repeat(64),
            size: 10,
            mode: 0o644,
            extract: true,
        }
    }

    #[test]
    fn an_artifact_must_stay_in_the_volume_and_name_its_content_exactly() {
        let ok = StepResult {
            artifacts: vec![artifact("game/bin")],
            pinned: true,
            resolved: String::new(),
        };
        assert_eq!(ok.problem(), None);
        for bad in ["/etc/passwd", "../up", "a/../../b", "", "a//b"] {
            assert!(artifact(bad).problem().is_some(), "{bad:?}");
        }
        let unhashed = StepArtifact { sha256: "ABC".into(), ..artifact("x") };
        assert!(unhashed.problem().is_some());
        let local = StepArtifact { url: "file:///etc/shadow".into(), ..artifact("x") };
        assert!(local.problem().is_some());
    }
}
