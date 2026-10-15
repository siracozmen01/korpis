// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an install is given (§4 and §6 of docs/09-recipes.md): which verbs run and which
//! templates render, each template's context built here, by the control plane, from declared
//! values only, and the check that everything an install writes lands in one of the workload's
//! volumes, because the worker that writes it sees nothing else (§6 of docs/02-architecture.md).

use super::RecipeError;

pub mod detail;
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
    context_of(schema, values, vars, |f| !f.secret)
}

/// The context of `vars` from `values`, each a field `reachable` admits.
pub(crate) fn context_of(
    schema: &ConfigSchema,
    values: &ValueMap,
    vars: &[String],
    reachable: impl Fn(&super::config::ConfigField) -> bool,
) -> Result<Context, RecipeError> {
    let mut out = Context::new();
    for name in vars {
        let field = schema
            .field(name)
            .filter(|f| reachable(f))
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

    /// The provider steps of its install: each one's index, step kind, and arguments.
    pub fn provider_steps(&self) -> impl Iterator<Item = (usize, &str, &ValueMap)> {
        self.install.iter().enumerate().filter_map(|(i, s)| match s {
            InstallStep::Provider { provider, args, .. } => Some((i, provider.as_str(), args)),
            _ => None,
        })
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
                | InstallStep::Template { to, .. }
                | InstallStep::Provider { to, .. } => Some((at("to"), to.as_str())),
                InstallStep::Mkdir { path, .. }
                | InstallStep::Write { path, .. }
                | InstallStep::Chmod { path, .. }
                | InstallStep::Chown { path, .. }
                | InstallStep::Delete { path } => Some((at("path"), path.as_str())),
                InstallStep::Symlink { link, .. } => Some((at("link"), link.as_str())),
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

    /// What an install runs, in order: every verb when `verbs`, then each of `templates` from
    /// the `templates` section, each template with its context from the intent's resolved
    /// `values`.
    ///
    /// # Errors
    /// A template's context cannot be built.
    pub fn install_actions(
        &self,
        values: &ValueMap,
        (verbs, templates): (bool, &[usize]),
    ) -> Result<Vec<InstallAction>, RecipeError> {
        let mut out = Vec::new();
        let ran = if verbs { self.install.as_slice() } else { &[] };
        for step in ran {
            let vars = match step {
                InstallStep::Template { vars, .. } => Some(vars),
                _ => None,
            };
            let context = vars.map(|v| context(&self.config, values, v)).transpose()?;
            out.push(InstallAction { step: step.clone(), context });
        }
        for t in templates.iter().filter_map(|i| self.templates.get(*i)) {
            let step = InstallStep::Template {
                from: t.from.clone(),
                to: t.path.clone(),
                format: t.format,
                vars: t.vars.clone(),
            };
            out.push(InstallAction {
                step,
                context: Some(context(&self.config, values, &t.vars)?),
            });
        }
        Ok(out)
    }

    /// The detail of an install step: why it runs, and the actions it runs (see
    /// [`Recipe::install_actions`]).
    ///
    /// # Errors
    /// A template's context cannot be built.
    pub fn install_detail(
        &self,
        values: &ValueMap,
        reason: InstallReason,
        which: (bool, &[usize]),
    ) -> Result<ValueMap, RecipeError> {
        let actions = self.install_actions(values, which)?;
        Ok(ValueMap::from([
            ("reason".to_owned(), Value::String(reason.as_str().to_owned())),
            ("actions".to_owned(), Value::List(actions.iter().map(detail::encode).collect())),
        ]))
    }
}

/// One thing an install does: a verb, and for a template, the context it renders from.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallAction {
    /// The verb and its arguments.
    pub step: InstallStep,
    /// A template's context; `None` for every other verb.
    pub context: Option<Context>,
}

/// The actions an install step's `detail` keeps.
///
/// # Errors
/// The detail holds no list of actions, or one of them is malformed.
pub fn actions_of(detail: &ValueMap) -> Result<Vec<InstallAction>, String> {
    match detail.get("actions") {
        Some(Value::List(list)) => list.iter().map(detail::decode).collect(),
        _ => Err("an install step's detail lists its actions".into()),
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
