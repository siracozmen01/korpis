// SPDX-License-Identifier: AGPL-3.0-or-later

//! Redaction before persistence (§7 of docs/14-streams.md): the values of a workload's secret
//! configuration fields, matched as literal bytes in each record before it is written, and
//! replaced by a mark naming the field. Best-effort, and said to be: a value the workload encodes
//! before printing it, or splits across two records, is not found.
//!
//! A value shorter than [`MIN_LEN`] bytes is not matched at all, because a two-letter secret
//! would redact every occurrence of two letters; it is reported back, so the interface can say
//! that field is not covered.

use aho_corasick::{AhoCorasick, MatchKind};
use std::borrow::Cow;

/// The shortest value matched.
pub const MIN_LEN: usize = 4;

/// The patterns of one stream.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    matcher: Option<AhoCorasick>,
    names: Vec<String>,
}

/// One secret: the field's name and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Secret {
    /// The configuration field.
    pub name: String,
    /// Its value.
    pub value: Vec<u8>,
}

impl Redactor {
    /// Matches `secrets`; also the names of those too short to match.
    pub fn new(secrets: &[Secret]) -> Result<(Self, Vec<String>), String> {
        let (usable, short): (Vec<_>, Vec<_>) =
            secrets.iter().partition(|s| s.value.len() >= MIN_LEN);
        let short = short.into_iter().map(|s| s.name.clone()).collect();
        if usable.is_empty() {
            return Ok((Self::default(), short));
        }
        let matcher = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(usable.iter().map(|s| &s.value))
            .map_err(|e| e.to_string())?;
        let names = usable.into_iter().map(|s| s.name.clone()).collect();
        Ok((Self { matcher: Some(matcher), names }, short))
    }

    /// The patterns in `json`: an array of `{"name": …, "value": …}`, values as strings.
    pub fn from_json(json: &[u8]) -> Result<(Self, Vec<String>), String> {
        let parsed: serde_json::Value = serde_json::from_slice(json).map_err(|e| e.to_string())?;
        let items = parsed.as_array().ok_or("an array of secrets")?;
        let secrets = items
            .iter()
            .map(|i| {
                let name = i.get("name").and_then(|v| v.as_str()).ok_or("a secret's name")?;
                let value = i.get("value").and_then(|v| v.as_str()).ok_or("a secret's value")?;
                Ok(Secret { name: name.to_owned(), value: value.as_bytes().to_vec() })
            })
            .collect::<Result<Vec<_>, &str>>()?;
        Self::new(&secrets)
    }

    /// `body` with every secret in it replaced by `████████ (redacted: <name>)`.
    #[must_use]
    pub fn apply<'a>(&self, body: &'a [u8]) -> Cow<'a, [u8]> {
        let Some(matcher) = &self.matcher else { return Cow::Borrowed(body) };
        let mut found = matcher.find_iter(body).peekable();
        if found.peek().is_none() {
            return Cow::Borrowed(body);
        }
        let mut out = Vec::with_capacity(body.len());
        let mut at = 0;
        for m in found {
            out.extend_from_slice(body.get(at..m.start()).unwrap_or_default());
            let name = self.names.get(m.pattern().as_usize()).map_or("secret", String::as_str);
            out.extend_from_slice(format!("████████ (redacted: {name})").as_bytes());
            at = m.end();
        }
        out.extend_from_slice(body.get(at..).unwrap_or_default());
        Cow::Owned(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(name: &str, value: &str) -> Secret {
        Secret { name: name.into(), value: value.as_bytes().to_vec() }
    }

    #[test]
    fn every_secret_is_replaced_by_its_name_and_short_ones_are_reported() {
        let (r, short) = Redactor::new(&[
            secret("rcon_password", "hunter22"),
            secret("api_key", "hunter22-extended"),
            secret("pin", "42"),
        ])
        .unwrap();
        assert_eq!(short, ["pin"]);
        let out = r.apply(b"RCON enabled, password hunter22; key hunter22-extended; 42");
        assert_eq!(
            String::from_utf8(out.into_owned()).unwrap(),
            "RCON enabled, password ████████ (redacted: rcon_password); key ████████ \
             (redacted: api_key); 42"
        );
        assert!(matches!(r.apply(b"nothing here"), Cow::Borrowed(_)));
        let (json, _) =
            Redactor::from_json(br#"[{"name":"db_url","value":"postgres://x"}]"#).unwrap();
        assert!(String::from_utf8_lossy(&json.apply(b"at postgres://x")).contains("db_url"));
    }
}
