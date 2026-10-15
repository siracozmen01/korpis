// SPDX-License-Identifier: AGPL-3.0-or-later

//! An install's actions as a plan step's detail keeps them: each verb with its arguments, and
//! each template with its context, in the order they run, so the plan shows exactly what will be
//! written, and the node that runs them needs nothing else of the recipe.

use super::InstallAction;
use crate::recipe::install::{Hash, InstallStep};
use crate::recipe::template::{Context, TemplateFormat};
use crate::value::{Value, ValueMap};

fn s(v: &str) -> Value {
    Value::String(v.to_owned())
}

fn n(v: u32) -> Value {
    Value::Int(i64::from(v))
}

fn strings(v: &[String]) -> Value {
    Value::List(v.iter().map(|x| s(x)).collect())
}

/// A format's name, as a recipe writes it.
#[must_use]
pub const fn format_name(f: TemplateFormat) -> &'static str {
    match f {
        TemplateFormat::Properties => "properties",
        TemplateFormat::Yaml => "yaml",
        TemplateFormat::Json => "json",
        TemplateFormat::Toml => "toml",
        TemplateFormat::Ini => "ini",
        TemplateFormat::Env => "env",
        TemplateFormat::Xml => "xml",
    }
}

fn format_named(name: &str) -> Option<TemplateFormat> {
    use TemplateFormat as F;
    [F::Properties, F::Yaml, F::Json, F::Toml, F::Ini, F::Env, F::Xml]
        .into_iter()
        .find(|f| format_name(*f) == name)
}

/// The verb's name and its arguments.
fn fields(step: &InstallStep) -> (&'static str, Vec<(&'static str, Value)>) {
    match step {
        InstallStep::Fetch { url, hash, to } => {
            ("fetch", vec![("url", s(url)), ("digest", s(&hash.to_digest())), ("to", s(to))])
        }
        InstallStep::Copy { from, to } => ("copy", vec![("from", s(from)), ("to", s(to))]),
        InstallStep::Extract { from, to, strip, include } => (
            "extract",
            vec![
                ("from", s(from)),
                ("to", s(to)),
                ("strip", n(*strip)),
                ("include", strings(include)),
            ],
        ),
        InstallStep::Mkdir { path, mode } => ("mkdir", vec![("path", s(path)), ("mode", n(*mode))]),
        InstallStep::Write { path, content, mode } => {
            ("write", vec![("path", s(path)), ("content", s(content)), ("mode", n(*mode))])
        }
        InstallStep::Template { from, to, format, vars } => (
            "template",
            vec![
                ("from", s(from)),
                ("to", s(to)),
                ("format", s(format_name(*format))),
                ("vars", strings(vars)),
            ],
        ),
        InstallStep::Chmod { path, mode } => ("chmod", vec![("path", s(path)), ("mode", n(*mode))]),
        InstallStep::Chown { path, uid, gid } => {
            ("chown", vec![("path", s(path)), ("uid", n(*uid)), ("gid", n(*gid))])
        }
        InstallStep::Symlink { target, link } => {
            ("symlink", vec![("target", s(target)), ("link", s(link))])
        }
        InstallStep::Delete { path } => ("delete", vec![("path", s(path))]),
        InstallStep::Provider { provider, to, args } => (
            "provider",
            vec![("provider", s(provider)), ("to", s(to)), ("args", Value::Map(args.clone()))],
        ),
    }
}

/// `a` as a detail value.
#[must_use]
pub fn encode(a: &InstallAction) -> Value {
    let (verb, args) = fields(&a.step);
    let mut m: ValueMap = args.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    m.insert("verb".into(), s(verb));
    if let Some(c) = &a.context {
        let kept = c.iter().map(|(k, v)| (k.clone(), super::var_value(v))).collect();
        m.insert("context".into(), Value::Map(kept));
    }
    Value::Map(m)
}

/// Reads one argument of a kept action.
struct Kept<'a>(&'a ValueMap);

impl Kept<'_> {
    fn text(&self, k: &str) -> Result<String, String> {
        match self.0.get(k) {
            Some(Value::String(v)) => Ok(v.clone()),
            _ => Err(format!("{k} is not text")),
        }
    }

    fn num(&self, k: &str) -> Result<u32, String> {
        match self.0.get(k) {
            Some(Value::Int(v)) => u32::try_from(*v).map_err(|_| format!("{k} is out of range")),
            _ => Err(format!("{k} is not a number")),
        }
    }

    fn list(&self, k: &str) -> Result<Vec<String>, String> {
        match self.0.get(k) {
            Some(Value::List(v)) => v
                .iter()
                .map(|x| match x {
                    Value::String(t) => Ok(t.clone()),
                    _ => Err(format!("{k} holds a non-text")),
                })
                .collect(),
            _ => Err(format!("{k} is not a list")),
        }
    }
}

fn step(verb: &str, k: &Kept<'_>) -> Result<InstallStep, String> {
    Ok(match verb {
        "fetch" => InstallStep::Fetch {
            url: k.text("url")?,
            hash: Hash::parse(&k.text("digest")?).ok_or("digest is malformed")?,
            to: k.text("to")?,
        },
        "copy" => InstallStep::Copy { from: k.text("from")?, to: k.text("to")? },
        "extract" => InstallStep::Extract {
            from: k.text("from")?,
            to: k.text("to")?,
            strip: k.num("strip")?,
            include: k.list("include")?,
        },
        "mkdir" => InstallStep::Mkdir { path: k.text("path")?, mode: k.num("mode")? },
        "write" => InstallStep::Write {
            path: k.text("path")?,
            content: k.text("content")?,
            mode: k.num("mode")?,
        },
        "template" => InstallStep::Template {
            from: k.text("from")?,
            to: k.text("to")?,
            format: format_named(&k.text("format")?).ok_or("format is unknown")?,
            vars: k.list("vars")?,
        },
        "chmod" => InstallStep::Chmod { path: k.text("path")?, mode: k.num("mode")? },
        "chown" => {
            InstallStep::Chown { path: k.text("path")?, uid: k.num("uid")?, gid: k.num("gid")? }
        }
        "symlink" => InstallStep::Symlink { target: k.text("target")?, link: k.text("link")? },
        "delete" => InstallStep::Delete { path: k.text("path")? },
        "provider" => InstallStep::Provider {
            provider: k.text("provider")?,
            to: k.text("to")?,
            args: match k.0.get("args") {
                Some(Value::Map(m)) => m.clone(),
                _ => return Err("args is not a map".into()),
            },
        },
        other => return Err(format!("{other:?} is not a verb")),
    })
}

/// The action a detail value keeps, checked as the recipe's steps are.
///
/// # Errors
/// It is not one, or its step does not validate.
pub fn decode(v: &Value) -> Result<InstallAction, String> {
    let Value::Map(m) = v else { return Err("an action is a map".into()) };
    let k = Kept(m);
    let step = step(&k.text("verb")?, &k)?;
    step.validate("action").map_err(|e| e.to_string())?;
    let context = match m.get("context") {
        None => None,
        Some(Value::Map(c)) => Some(
            c.iter()
                .map(|(name, v)| super::value_var(v).map(|x| (name.clone(), x)))
                .collect::<Option<Context>>()
                .ok_or("a context value is malformed")?,
        ),
        Some(_) => return Err("context is not a map".into()),
    };
    Ok(InstallAction { step, context })
}
