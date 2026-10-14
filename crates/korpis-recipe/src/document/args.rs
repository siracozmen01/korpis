// SPDX-License-Identifier: AGPL-3.0-or-later

//! Typed access to the free-form maps install steps and probes are written as, refusing keys
//! the verb does not take.

use korpis_core::recipe::RecipeError;
use serde_json::{Map, Value};

/// A step's or probe's arguments, with the path they were found at.
pub struct Args<'a> {
    pub at: String,
    map: &'a Map<String, Value>,
}

impl<'a> Args<'a> {
    pub fn new(at: String, v: &'a Value) -> Result<Self, RecipeError> {
        match v {
            Value::Object(map) => Ok(Self { at, map }),
            _ => Err(RecipeError::new(&at, "a map of arguments")),
        }
    }

    pub fn from_map(at: String, map: &'a Map<String, Value>) -> Self {
        Self { at, map }
    }

    pub fn err(&self, key: &str, message: impl Into<String>) -> RecipeError {
        RecipeError::new(&self.at(key), message)
    }

    /// Where `key` is, for an error about it.
    pub fn at(&self, key: &str) -> String {
        format!("{}.{key}", self.at)
    }

    /// Refuses any key outside `allowed`.
    pub fn only(&self, allowed: &[&str]) -> Result<(), RecipeError> {
        match self.map.keys().find(|k| !allowed.contains(&k.as_str())) {
            Some(k) => {
                Err(self.err(k, format!("not an argument here; expected one of {allowed:?}")))
            }
            None => Ok(()),
        }
    }

    pub fn raw(&self, key: &str) -> Option<&'a Value> {
        self.map.get(key)
    }

    pub fn opt_str(&self, key: &str) -> Result<Option<String>, RecipeError> {
        match self.map.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(self.err(key, "a string")),
        }
    }

    pub fn str(&self, key: &str) -> Result<String, RecipeError> {
        self.opt_str(key)?.ok_or_else(|| self.err(key, "required"))
    }

    pub fn opt_u32(&self, key: &str) -> Result<Option<u32>, RecipeError> {
        match self.map.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .map(Some)
                .ok_or_else(|| self.err(key, "a whole number")),
        }
    }

    /// A file mode, written as an octal string (`"0755"`) or a number.
    pub fn mode(&self, key: &str, default: u32) -> Result<u32, RecipeError> {
        match self.map.get(key) {
            None => Ok(default),
            Some(Value::String(s)) => u32::from_str_radix(s.trim_start_matches("0o"), 8)
                .map_err(|_| self.err(key, "an octal mode")),
            Some(v) => v
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| self.err(key, "a mode")),
        }
    }

    pub fn strings(&self, key: &str) -> Result<Vec<String>, RecipeError> {
        match self.map.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| {
                    v.as_str().map(str::to_owned).ok_or_else(|| self.err(key, "a list of strings"))
                })
                .collect(),
            Some(_) => Err(self.err(key, "a list of strings")),
        }
    }
}
