// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an install is given (§4 and §6 of docs/09-recipes.md): which verbs run and which
//! templates render, each template's context built here, by the control plane, from declared
//! values only, and the check that everything an install writes lands in one of the workload's
//! volumes, because the worker that writes it sees nothing else (§6 of docs/02-architecture.md).

use super::RecipeError;
use super::config::{ConfigSchema, ConfigType};
use super::deliver::text;
use super::document::Recipe;
use super::install::InstallStep;
use super::template::{Context, Var};
use crate::value::{Value, ValueMap};
use crate::workload::spec::VolumeSpec;

/// Why an install runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallReason {
    /// The workload is created: every verb, then every template.
    Create,
    /// Its recipe changed: the new recipe's verbs and templates, over what is there.
    Recipe,
    /// Values its templates use changed: those templates, rendered again.
    Config,
}

impl InstallReason {
    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Recipe => "recipe",
            Self::Config => "config",
        }
    }
}

/// The context of a template listing `vars`, from the resolved `values` of the intent. Every
/// name is a declared field delivered through templates and never a secret (checked when the
/// recipe loads, §5.2 of docs/09-recipes.md); this checks it again, because a context is the
/// one thing a template can reach.
pub fn context(
    schema: &ConfigSchema,
    values: &ValueMap,
    vars: &[String],
) -> Result<Context, RecipeError> {
    let mut out = Context::new();
    for name in vars {
        let field = schema
            .field(name)
            .filter(|f| !f.secret)
            .ok_or_else(|| RecipeError::new(name, "not a template variable"))?;
        let var = match values.get(name) {
            None | Some(Value::Null) => Var::Unset,
            Some(v) => match field.ty {
                ConfigType::String | ConfigType::Enum => {
                    Var::Text(text(field.ty, v).map_err(|e| RecipeError::new(name, e))?)
                }
                ConfigType::Boolean => Var::Bool(matches!(v, Value::Bool(true))),
                _ => Var::Number(text(field.ty, v).map_err(|e| RecipeError::new(name, e))?),
            },
        };
        out.insert(name.clone(), var);
    }
    Ok(out)
}

/// `var` as a plan step's detail keeps it: the kind it is written as, and its text.
#[must_use]
pub fn var_value(var: &Var) -> Value {
    let one = |k: &str, v: Value| Value::Map([(k.to_owned(), v)].into());
    match var {
        Var::Text(s) => one("text", Value::String(s.clone())),
        Var::Number(n) => one("number", Value::String(n.clone())),
        Var::Bool(b) => one("bool", Value::Bool(*b)),
        Var::Unset => Value::Null,
    }
}

/// The variable a step's detail keeps as `v`.
#[must_use]
pub fn value_var(v: &Value) -> Option<Var> {
    let Value::Map(m) = v else { return (v == &Value::Null).then_some(Var::Unset) };
    match m.iter().next()? {
        (k, Value::String(s)) if k == "text" => Some(Var::Text(s.clone())),
        (k, Value::String(n)) if k == "number" => Some(Var::Number(n.clone())),
        (k, Value::Bool(b)) if k == "bool" => Some(Var::Bool(*b)),
        _ => None,
    }
}

impl Recipe {
    /// Whether it installs anything: a verb or a template.
    #[must_use]
    pub fn installs(&self) -> bool {
        !self.install.is_empty() || !self.templates.is_empty()
    }

    /// Every path an install writes, with where the recipe says it.
    #[must_use]
    pub fn install_targets(&self) -> Vec<(String, &str)> {
        let mut out = Vec::new();
        for (i, s) in self.install.iter().enumerate() {
            let at = |f: &str| format!("install.steps[{i}].{f}");
            let target = match s {
                InstallStep::Fetch { to, .. }
                | InstallStep::Copy { to, .. }
                | InstallStep::Extract { to, .. }
                | InstallStep::Template { to, .. } => Some((at("to"), to.as_str())),
                InstallStep::Mkdir { path, .. }
                | InstallStep::Write { path, .. }
                | InstallStep::Chmod { path, .. }
                | InstallStep::Chown { path, .. }
                | InstallStep::Delete { path } => Some((at("path"), path.as_str())),
                InstallStep::Symlink { link, .. } => Some((at("link"), link.as_str())),
                // Its targets are known once it is resolved, and checked then.
                InstallStep::Provider { .. } => None,
            };
            out.extend(target);
        }
        for (i, t) in self.templates.iter().enumerate() {
            out.push((format!("templates[{i}].path"), t.path.as_str()));
        }
        out
    }

    /// Checks that every path an install writes lies in one of `volumes`: the volume's own path
    /// or beneath it.
    ///
    /// # Errors
    /// The first path that lies in none of them.
    pub fn check_targets(&self, volumes: &[VolumeSpec]) -> Result<(), RecipeError> {
        for (at, path) in self.install_targets() {
            if !volumes.iter().any(|v| within(path, &v.path)) {
                let why = format!("{path} is in no volume: an install writes only into volumes");
                return Err(RecipeError::new(&at, why));
            }
        }
        Ok(())
    }

    /// The templates of the `templates` section that use any of `keys`.
    #[must_use]
    pub fn templates_using(&self, keys: &[String]) -> Vec<usize> {
        let uses = |vars: &[String]| vars.iter().any(|v| keys.contains(v));
        self.templates.iter().enumerate().filter(|(_, t)| uses(&t.vars)).map(|(i, _)| i).collect()
    }

    /// The detail of an install step: why it runs, whether the verbs do, which templates render,
    /// and the context of each template it renders, from the intent's resolved `values`.
    ///
    /// # Errors
    /// A template's context cannot be built.
    pub fn install_detail(
        &self,
        values: &ValueMap,
        reason: InstallReason,
        (verbs, templates): (bool, &[usize]),
    ) -> Result<ValueMap, RecipeError> {
        let mut contexts = ValueMap::new();
        let mut keep = |key: String, vars: &[String]| -> Result<(), RecipeError> {
            let c = context(&self.config, values, vars)?;
            contexts.insert(
                key,
                Value::Map(c.iter().map(|(k, v)| (k.clone(), var_value(v))).collect()),
            );
            Ok(())
        };
        let verb_templates = self.install.iter().enumerate().filter_map(|(i, s)| match s {
            InstallStep::Template { vars, .. } if verbs => Some((i, vars)),
            _ => None,
        });
        for (i, vars) in verb_templates {
            keep(format!("install.{i}"), vars)?;
        }
        for &i in templates {
            if let Some(t) = self.templates.get(i) {
                keep(format!("templates.{i}"), &t.vars)?;
            }
        }
        let index = |i: usize| Value::Int(i64::try_from(i).unwrap_or(i64::MAX));
        Ok(ValueMap::from([
            ("reason".to_owned(), Value::String(reason.as_str().to_owned())),
            ("verbs".to_owned(), Value::Bool(verbs)),
            ("templates".to_owned(), Value::List(templates.iter().copied().map(index).collect())),
            ("contexts".to_owned(), Value::Map(contexts)),
        ]))
    }
}

/// Whether `path` is `volume` or beneath it, component by component.
fn within(path: &str, volume: &str) -> bool {
    let volume = volume.trim_end_matches('/');
    path == volume || path.strip_prefix(volume).is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::within;

    #[test]
    fn a_path_is_within_a_volume_by_whole_components() {
        assert!(within("/data", "/data"));
        assert!(within("/data/server.jar", "/data"));
        assert!(within("/data/x", "/data/"));
        assert!(!within("/database/x", "/data"), "a prefix of a name is not a parent");
        assert!(!within("/etc/passwd", "/data"));
    }
}
