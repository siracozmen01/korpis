// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's managed files (§5.2 of docs/09-recipes.md), rendered each time its runtime object
//! is built, so a change of value reaches the software at its next start as every other setting
//! does. Each is rendered from the recipe's template, with the context only its own fields make
//! (§6), and written into `config/` of the workload's node directory, which the workload sees
//! read-only at `/run/korpis`. A file the recipe no longer declares is removed.
//!
//! The agent renders rather than a worker: the template engine reads nothing but its source and
//! its context, and the file is written by name into a directory of the node's, so there is no
//! path for a template to escape by.

use super::Reconciler;
use super::act::Failure;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_core::recipe::Recipe;
use korpis_core::recipe::managed::{ManagedFile, context};
use korpis_core::recipe::template::{MAX_SOURCE, Template};
use korpis_core::value::ValueMap;
use korpis_proto::korpis::agent::v1 as agent;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

fn failed(e: impl std::fmt::Display) -> Failure {
    Failure(format!("managed files: {e}"))
}

/// Each of `recipe`'s managed files, by name, rendered from `values`; `source` gives a template's
/// text by its path in the recipe.
pub(super) fn render(
    recipe: &Recipe,
    values: &ValueMap,
    source: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, Failure> {
    recipe
        .managed_files
        .iter()
        .map(|f: &ManagedFile| {
            let text =
                source.get(&f.from).ok_or_else(|| failed(format!("{} is missing", f.from)))?;
            let template = Template::parse(text).map_err(|e| failed(format!("{}: {e}", f.from)))?;
            let c = context(&recipe.config, values, f).map_err(failed)?;
            let rendered =
                template.render(f.format, &c).map_err(|e| failed(format!("{}: {e}", f.name)))?;
            Ok((f.name.clone(), rendered))
        })
        .collect()
}

/// Writes `files` into `config/` of `dir`, each read-only and replaced whole, and removes
/// whatever else is there. A name that is not a plain file name is refused here too, whatever
/// the recipe's loader said: nothing written lands outside `config/`.
pub(super) fn write(dir: &Path, files: &[(String, String)]) -> std::io::Result<()> {
    if let Some((name, _)) = files.iter().find(|(n, _)| !korpis_core::recipe::managed::plain(n)) {
        let why = format!("managed file name {name:?} is not a plain file name");
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, why));
    }
    let config = dir.join("config");
    if files.is_empty() {
        return match std::fs::remove_dir_all(&config) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(&config)?;
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o755))?;
    for entry in std::fs::read_dir(&config)? {
        let entry = entry?;
        let name = entry.file_name();
        if !files.iter().any(|(n, _)| std::ffi::OsStr::new(n) == name) {
            std::fs::remove_file(entry.path())?;
        }
    }
    for (name, text) in files {
        let path = config.join(name);
        crate::durable::replace(&path, text.as_bytes())?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))?;
    }
    Ok(())
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Renders `a`'s managed files into its node directory `dir`, or `None` when the node gives
    /// its workloads none: then a recipe that declares one is refused rather than started without
    /// a setting it promised.
    pub(super) async fn managed(
        &self,
        a: &agent::Assignment,
        dir: Option<&Path>,
    ) -> Result<(), Failure> {
        let recipe = korpis_recipe::parse(&a.recipe_manifest).map_err(failed)?;
        if recipe.managed_files.is_empty() {
            return dir.map_or(Ok(()), |d| write(d, &[]).map_err(failed));
        }
        let dir = dir.ok_or_else(|| failed("this node gives its workloads no directory"))?;
        let body = a.target.as_option().and_then(|i| i.body.as_option());
        let body = body.ok_or_else(|| failed("the assignment carries no target"))?;
        let r = body.recipe.as_option().ok_or_else(|| failed("the target names no recipe"))?;
        let files = self.host.fetch_files(&format!("{}@{}", r.name, r.digest)).await?;
        let mut source = BTreeMap::new();
        for f in &recipe.managed_files {
            let digest =
                files.get(&f.from).ok_or_else(|| failed(format!("{} is missing", f.from)))?;
            let bytes = self.host.read_content(digest, MAX_SOURCE).await?;
            let text =
                String::from_utf8(bytes).map_err(|_| failed(format!("{} is not UTF-8", f.from)))?;
            source.insert(f.from.clone(), text);
        }
        let values = body
            .config
            .iter()
            .map(|(k, v)| Ok((k.clone(), korpis_wire::value::from_pb(k, v).map_err(failed)?)))
            .collect::<Result<ValueMap, Failure>>()?;
        write(dir, &render(&recipe, &values, &source)?).map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        reason = "a failing test should fail loudly"
    )]

    use super::*;
    use korpis_core::value::Value;

    const RECIPE: &str = r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {name: hello, version: "1.0.0", description: hello, license: MIT}
runtime:
  targets:
    - {tier: container, arch: [amd64, arm64], image: "example.org/hello@sha256:0000000000000000000000000000000000000000000000000000000000000000"}
config:
  fields:
    - {key: max_view, type: integer, default: 10, permission: operator, deliver: managed_file}
    - {key: token, type: string, secret: true, permission: operator, deliver: managed_file, pattern: '[^\r\n]*'}
managed_files:
  - {name: limits.env, from: managed/limits.env, format: env, vars: [max_view, token]}
"#;

    #[test]
    fn a_managed_file_is_rendered_written_read_only_and_a_stale_one_removed() {
        let recipe = korpis_recipe::parse(RECIPE.as_bytes()).unwrap();
        let source = BTreeMap::from([(
            "managed/limits.env".to_owned(),
            "MAX_VIEW={{max_view}}\nTOKEN={{token}}\n".to_owned(),
        )]);
        let values = ValueMap::from([
            ("max_view".to_owned(), Value::Int(16)),
            ("token".to_owned(), Value::String("a\"b".into())),
        ]);
        let files = render(&recipe, &values, &source).unwrap();
        assert_eq!(
            files,
            [("limits.env".to_owned(), "MAX_VIEW=16\nTOKEN=\"a\\\"b\"\n".to_owned())]
        );

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(dir.path().join("config/old.env"), "x").unwrap();
        write(dir.path(), &files).unwrap();
        let path = dir.path().join("config/limits.env");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), files[0].1);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o444);
        assert!(
            !dir.path().join("config/old.env").exists(),
            "what the recipe no longer declares goes"
        );
        write(dir.path(), &files).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            files[0].1,
            "rewritten over its read-only self"
        );
        write(dir.path(), &[]).unwrap();
        assert!(!dir.path().join("config").exists(), "and with none declared, the directory");

        assert!(render(&recipe, &values, &BTreeMap::new()).is_err(), "a template the recipe lacks");
        for name in ["../escape", "a/b", "..", ""] {
            let e = write(dir.path(), &[(name.to_owned(), "x".to_owned())]).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "{name:?}");
        }
        assert!(!dir.path().join("escape").exists(), "nothing lands outside config/");
    }
}
