// SPDX-License-Identifier: AGPL-3.0-or-later

//! An extension's actions, `ext.<name>.<action>`: what an invocation carries to the extension,
//! what it answers, and how the arguments it is given are judged against what it declared. A
//! resource argument names an object the invoker is authorized on, and only those objects are
//! what the extension may act on for the invoker; an action taking none is authorized, and acts,
//! where the extension is installed.

use super::{ActionArgument, ArgumentType};
use crate::authz::{Kind, ObjectRef};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The longest message an action answers with: a line for a person, not a payload.
pub const MAX_MESSAGE: usize = 4096;

/// The most data an action answers with, as JSON text, for a client to render.
pub const MAX_DATA: usize = 64 * 1024;

/// The most arguments an invocation carries.
pub const MAX_ARGUMENTS: usize = 64;

/// The longest text argument.
pub const MAX_TEXT: usize = 4096;

/// One argument's value, of the type its declaration names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgumentValue {
    /// Text.
    Text(String),
    /// A whole number.
    Int(i64),
    /// True or false.
    Flag(bool),
    /// A workload, authorized for the invoker.
    Workload(Uuid),
    /// A project, authorized for the invoker.
    Project(Uuid),
}

impl ArgumentValue {
    /// The type it is a value of.
    #[must_use]
    pub const fn kind(&self) -> ArgumentType {
        match self {
            Self::Text(_) => ArgumentType::String,
            Self::Int(_) => ArgumentType::Int,
            Self::Flag(_) => ArgumentType::Bool,
            Self::Workload(_) => ArgumentType::Workload,
            Self::Project(_) => ArgumentType::Project,
        }
    }

    /// The object it names, for a resource.
    #[must_use]
    pub const fn object(&self) -> Option<ObjectRef> {
        match self {
            Self::Workload(id) => Some(ObjectRef::new(Kind::Workload, *id)),
            Self::Project(id) => Some(ObjectRef::new(Kind::Project, *id)),
            Self::Text(_) | Self::Int(_) | Self::Flag(_) => None,
        }
    }
}

/// An invocation's arguments, by name.
pub type Arguments = BTreeMap<String, ArgumentValue>;

/// Why arguments do not fit what an action declared: the argument, and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("argument {argument:?}: {reason}")]
pub struct ArgumentError {
    /// The argument; empty for the set as a whole.
    pub argument: String,
    /// What is wrong.
    pub reason: String,
}

impl ArgumentError {
    fn new(argument: &str, reason: impl Into<String>) -> Self {
        Self { argument: argument.to_owned(), reason: reason.into() }
    }
}

/// Checks `given` against `declared`: every required argument present, none undeclared, each of
/// its declared type, text no longer than [`MAX_TEXT`], at most [`MAX_ARGUMENTS`].
///
/// # Errors
/// The first argument that does not fit.
pub fn check(declared: &[ActionArgument], given: &Arguments) -> Result<(), ArgumentError> {
    if given.len() > MAX_ARGUMENTS {
        return Err(ArgumentError::new("", format!("at most {MAX_ARGUMENTS}")));
    }
    if let Some(missing) = declared.iter().find(|a| a.required && !given.contains_key(&a.name)) {
        return Err(ArgumentError::new(&missing.name, "required by the action"));
    }
    for (name, value) in given {
        let Some(d) = declared.iter().find(|a| &a.name == name) else {
            return Err(ArgumentError::new(name, "not an argument the action declares"));
        };
        if d.kind != value.kind() {
            return Err(ArgumentError::new(name, format!("a {}", d.kind.as_str())));
        }
        if matches!(value, ArgumentValue::Text(t) if t.len() > MAX_TEXT) {
            return Err(ArgumentError::new(name, format!("at most {MAX_TEXT} bytes")));
        }
    }
    Ok(())
}

/// The objects `given` names, each once, in argument order: what the invoker is authorized on and
/// the extension may act on for them.
#[must_use]
pub fn resources(given: &Arguments) -> Vec<ObjectRef> {
    let mut out: Vec<ObjectRef> = Vec::new();
    for o in given.values().filter_map(ArgumentValue::object) {
        if !out.contains(&o) {
            out.push(o);
        }
    }
    out
}

/// What an extension is sent to perform one of its actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invoke {
    /// The part after `ext.<extension>.`.
    pub action: String,
    /// Its arguments, checked against its declaration.
    pub arguments: Arguments,
    /// The invocation token, presented beside the extension's own credential on the calls it
    /// makes for the invoker; it expires when the invocation finishes.
    pub invocation: String,
    /// Who invoked it, as a principal's key.
    pub invoker: String,
}

/// What an action answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    /// A line for the person who invoked it.
    pub message: String,
    /// What it returns for a client to render, as JSON text; empty for nothing.
    pub data: String,
}

impl ActionResult {
    /// Whether it is within bounds: a message of at most [`MAX_MESSAGE`] bytes, and data empty or
    /// one JSON value of at most [`MAX_DATA`] bytes.
    #[must_use]
    pub fn well_formed(&self) -> bool {
        self.message.len() <= MAX_MESSAGE
            && self.data.len() <= MAX_DATA
            && (self.data.is_empty()
                || serde_json::from_str::<serde_json::Value>(&self.data).is_ok())
    }
}

/// One invocation as the control plane keeps it while it runs: the token that lets the extension
/// act for the invoker is valid only until it finishes or expires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Its identifier.
    pub id: Uuid,
    /// The installation invoked.
    pub extension: Uuid,
    /// The full action, `ext.<name>.<action>`.
    pub action: String,
    /// Who invoked it, as a principal's key.
    pub invoker: String,
    /// The objects the extension may act on for the invoker.
    pub resources: Vec<ObjectRef>,
    /// When the token stops being accepted, whatever else happens.
    pub expires_at: std::time::SystemTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared() -> Vec<ActionArgument> {
        vec![
            ActionArgument { name: "server".into(), kind: ArgumentType::Workload, required: true },
            ActionArgument { name: "reason".into(), kind: ArgumentType::String, required: false },
        ]
    }

    #[test]
    fn arguments_are_judged_by_what_the_action_declares() {
        let w = Uuid::now_v7();
        let mut given = Arguments::new();
        given.insert("server".into(), ArgumentValue::Workload(w));
        assert_eq!(check(&declared(), &given), Ok(()));
        assert_eq!(resources(&given), [ObjectRef::new(Kind::Workload, w)]);

        let missing = check(&declared(), &Arguments::new()).unwrap_err();
        assert_eq!(missing.argument, "server");
        given.insert("reason".into(), ArgumentValue::Int(3));
        assert_eq!(check(&declared(), &given).unwrap_err().argument, "reason", "of its type");
        given.insert("reason".into(), ArgumentValue::Text("x".repeat(MAX_TEXT + 1)));
        assert_eq!(check(&declared(), &given).unwrap_err().argument, "reason", "bounded");
        given.remove("reason");
        given.insert("other".into(), ArgumentValue::Flag(true));
        assert_eq!(check(&declared(), &given).unwrap_err().argument, "other", "undeclared");
    }

    #[test]
    fn a_result_is_a_bounded_line_and_json_or_nothing() {
        let ok = ActionResult { message: "kicked".into(), data: "{\"players\":3}".into() };
        assert!(ok.well_formed());
        assert!(ActionResult { message: "x".into(), data: String::new() }.well_formed());
        assert!(!ActionResult { message: "x".into(), data: "{".into() }.well_formed());
        let long = ActionResult { message: "x".repeat(MAX_MESSAGE + 1), data: String::new() };
        assert!(!long.well_formed());
    }
}
