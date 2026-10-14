// SPDX-License-Identifier: AGPL-3.0-or-later

//! Encoding a value for the format it is placed in (§6 of docs/09-recipes.md), so that no value
//! can end the line, the string, or the element it was placed in. Text is written as a whole
//! token of the format: a quoted string in JSON, YAML, TOML, and env files, escaped character data
//! in XML, an escaped value in properties. What a format cannot carry is refused, never dropped:
//! a line break in INI, a character XML 1.0 does not allow.

use super::{TemplateFormat, Var};
use std::fmt::Write as _;

/// `v` as `format` writes it, or why it cannot be.
pub(super) fn encode(format: TemplateFormat, v: &Var) -> Result<String, String> {
    use TemplateFormat as F;
    Ok(match v {
        Var::Unset => match format {
            F::Json | F::Yaml => "null".into(),
            F::Toml | F::Env => "\"\"".into(),
            F::Properties | F::Ini | F::Xml => String::new(),
        },
        Var::Bool(b) => b.to_string(),
        Var::Number(n) => n.clone(),
        Var::Text(s) => match format {
            F::Json => quoted(s, json_char),
            F::Yaml | F::Toml => quoted(s, |c, o| unicode_escape(c, o, format == F::Toml)),
            F::Env => env(s)?,
            F::Properties => properties(s),
            F::Ini => ini(s)?,
            F::Xml => xml(s)?,
        },
    })
}

/// `s` in double quotes, each character written by `put`.
fn quoted(s: &str, put: impl Fn(char, &mut String)) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        put(c, &mut out);
    }
    out.push('"');
    out
}

fn json_char(c: char, out: &mut String) {
    match c {
        '"' => out.push_str("\\\""),
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\t' => out.push_str("\\t"),
        '\r' => out.push_str("\\r"),
        c if u32::from(c) < 0x20 => {
            let _ = write!(out, "\\u{:04x}", u32::from(c));
        }
        c => out.push(c),
    }
}

/// YAML and TOML double-quoted strings: both take `\uXXXX`, and both treat more than ASCII
/// controls as breaks or forbid them, so those are escaped too.
fn unicode_escape(c: char, out: &mut String, toml: bool) {
    match c {
        '"' => out.push_str("\\\""),
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\t' => out.push_str("\\t"),
        '\r' => out.push_str("\\r"),
        c if u32::from(c) < 0x20 || c == '\u{7f}' => {
            let _ = write!(out, "\\u{:04X}", u32::from(c));
        }
        // YAML reads these as line breaks; TOML allows them.
        '\u{85}' | '\u{2028}' | '\u{2029}' if !toml => {
            let _ = write!(out, "\\u{:04X}", u32::from(c));
        }
        c => out.push(c),
    }
}

/// An env file's value, double-quoted with backslash escapes as POSIX shells and dotenv parsers
/// read them; a line break cannot be carried by every reader, so it is refused.
fn env(s: &str) -> Result<String, String> {
    if s.contains(['\n', '\r', '\0']) {
        return Err("an env file value is one line".into());
    }
    Ok(quoted(s, |c, out| {
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }))
}

/// A Java properties value: backslash escapes for what the format reads specially, and `\uXXXX`
/// for anything outside printable ASCII, so the file reads the same as ISO-8859-1 or UTF-8.
fn properties(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{c}' => out.push_str("\\f"),
            // Leading white space is otherwise taken for the separator's.
            ' ' if i == 0 => out.push_str("\\ "),
            c if (' '..='~').contains(&c) => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{u:04X}");
                }
            }
        }
    }
    out
}

/// An INI value has no escapes; what it cannot hold is refused.
fn ini(s: &str) -> Result<String, String> {
    if s.contains(['\n', '\r', '\0']) {
        return Err("an INI value is one line".into());
    }
    Ok(s.to_owned())
}

/// XML character data, safe in an element or an attribute value in either quote.
fn xml(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if u32::from(c) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {
                return Err(format!("XML 1.0 has no character U+{:04X}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    Ok(out)
}
