// SPDX-License-Identifier: AGPL-3.0-or-later

//! The template language (§6 of docs/09-recipes.md): text, `{{name}}` substitution, a section
//! `{{#name}}…{{/name}}` rendered when the value is present and not false or empty, its inverse
//! `{{^name}}…{{/name}}`, and a comment `{{! … }}`. Nothing else: no loops, no traversal, no
//! calls, no includes, no partials, no change of delimiters.

use super::TemplateError;

/// The largest template source read, in bytes.
pub const MAX_SOURCE: usize = 1 << 20;
/// How deeply sections may nest.
const MAX_DEPTH: usize = 16;

/// One piece of a parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Node {
    /// Literal text.
    Text(String),
    /// A value substituted, encoded for the format.
    Var(String),
    /// A section: its body rendered when the value is truthy, or falsy when `inverted`.
    Section {
        /// The value it tests.
        name: String,
        /// Rendered when the value is falsy instead.
        inverted: bool,
        /// What it holds.
        body: Vec<Node>,
    },
}

/// Whether `name` is a field key: `[a-z][a-z0-9_]*`, at most 64 bytes.
fn key(name: &str) -> bool {
    name.len() <= 64
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Parses `src` into its nodes.
pub(super) fn parse(src: &str) -> Result<Vec<Node>, TemplateError> {
    if src.len() > MAX_SOURCE {
        return Err(TemplateError::new(0, "a template is at most 1 MiB"));
    }
    // Each open section: its name, whether inverted, and the nodes before it.
    let mut open: Vec<(String, bool, Vec<Node>, usize)> = Vec::new();
    let mut nodes = Vec::new();
    let mut rest = src;
    while let Some(start) = rest.find("{{") {
        let at = src.len() - rest.len() + start;
        if start > 0 {
            nodes.push(Node::Text(rest[..start].to_owned()));
        }
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or_else(|| TemplateError::new(at, "an unclosed {{"))?;
        let tag = after[..end].trim();
        rest = &after[end + 2..];
        let (sigil, name) = match tag.chars().next() {
            Some(c @ ('#' | '^' | '/' | '!')) => (Some(c), tag[1..].trim()),
            _ => (None, tag),
        };
        if sigil == Some('!') {
            continue;
        }
        if !key(name) {
            return Err(TemplateError::new(at, format!("{tag:?} names no value")));
        }
        match sigil {
            None => nodes.push(Node::Var(name.to_owned())),
            Some(c @ ('#' | '^')) => {
                if open.len() >= MAX_DEPTH {
                    return Err(TemplateError::new(at, "sections nest at most 16 deep"));
                }
                open.push((name.to_owned(), c == '^', std::mem::take(&mut nodes), at));
            }
            _ => {
                let Some((opened, inverted, before, _)) = open.pop() else {
                    return Err(TemplateError::new(at, format!("{{{{/{name}}}}} closes nothing")));
                };
                if opened != name {
                    let why = format!("{{{{/{name}}}}} closes the section {opened:?}");
                    return Err(TemplateError::new(at, why));
                }
                let body = std::mem::replace(&mut nodes, before);
                nodes.push(Node::Section { name: opened, inverted, body });
            }
        }
    }
    if let Some((name, _, _, at)) = open.pop() {
        return Err(TemplateError::new(at, format!("the section {name:?} is never closed")));
    }
    if !rest.is_empty() {
        nodes.push(Node::Text(rest.to_owned()));
    }
    Ok(nodes)
}
