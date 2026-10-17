// SPDX-License-Identifier: AGPL-3.0-or-later

//! The strings a recipe shows (§5.3 of docs/09-recipes.md): a field's label and the recipe's
//! description are messages in the recipe's default locale, and a catalogue translates them into
//! another, keyed by where the message is in the document. A message is ICU `MessageFormat` with
//! no argument: the panel formats these with no values, so a placeholder would show as a failure
//! rather than as anything the recipe meant, and is refused where the recipe loads instead. What
//! is left of the syntax is its quoting, which is what the panel's formatter reads it by: `''` is
//! one apostrophe, an apostrophe before `{` or `}` quotes up to the next lone apostrophe, and an
//! unquoted brace is a placeholder.

use super::{Recipe, RecipeError};
use std::collections::BTreeMap;

/// A catalogue: a message's key, its translation.
pub type Catalogue = BTreeMap<String, String>;

/// The longest message accepted, in bytes.
pub const MAX_MESSAGE: usize = 4096;

/// The key of the recipe's description.
pub const DESCRIPTION: &str = "metadata.description";

/// The key of field `key`'s label.
#[must_use]
pub fn label_key(key: &str) -> String {
    format!("config.fields.{key}.label")
}

/// Checks that `text` is a message the panel can show as it is meant: bounded, and with no
/// placeholder outside a quotation.
pub fn check_message(field: &str, text: &str) -> Result<(), RecipeError> {
    if text.len() > MAX_MESSAGE {
        return Err(RecipeError::new(field, format!("at most {MAX_MESSAGE} bytes")));
    }
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('\'', _) if chars.peek() == Some(&'\'') => {
                chars.next();
            }
            ('\'', true) => quoted = false,
            ('\'', false) if matches!(chars.peek(), Some('{' | '}')) => quoted = true,
            ('{' | '}', false) => {
                return Err(RecipeError::new(
                    field,
                    "no placeholder: nothing fills one; quote a brace as '{' to show it",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Checks that `tag` is a BCP 47 language tag of the plain form a catalogue's file is named by:
/// a language of two or three letters, then subtags of letters and digits.
pub fn check_locale(field: &str, tag: &str) -> Result<(), RecipeError> {
    let mut parts = tag.split('-');
    let language = parts.next().unwrap_or_default();
    let language_ok =
        (2..=3).contains(&language.len()) && language.chars().all(|c| c.is_ascii_alphabetic());
    let rest_ok =
        parts.all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()));
    if language_ok && rest_ok && tag.len() <= 35 {
        Ok(())
    } else {
        Err(RecipeError::new(field, format!("{tag:?} is not a language tag such as en or pt-BR")))
    }
}

impl Recipe {
    /// Every message the recipe shows, by key, in its default locale.
    #[must_use]
    pub fn messages(&self) -> BTreeMap<String, &str> {
        let mut out = BTreeMap::new();
        if !self.metadata.description.is_empty() {
            out.insert(DESCRIPTION.to_owned(), self.metadata.description.as_str());
        }
        for f in self.config.fields() {
            if !f.label.is_empty() {
                out.insert(label_key(&f.key), f.label.as_str());
            }
        }
        out
    }

    /// Checks the recipe's own messages and its default locale.
    pub(super) fn check_messages(&self) -> Result<(), RecipeError> {
        check_locale("metadata.locale", &self.metadata.locale)?;
        self.messages().iter().try_for_each(|(key, text)| check_message(key, text))
    }

    /// Checks `catalogue`, the translation into `locale`: a locale other than the recipe's own,
    /// whose every key is a message the recipe shows and every value a message, so a
    /// translation can neither add a string the recipe never showed nor break one it did.
    pub fn check_catalogue(&self, locale: &str, catalogue: &Catalogue) -> Result<(), RecipeError> {
        let at = format!("i18n/{locale}.json");
        check_locale(&at, locale)?;
        if locale.eq_ignore_ascii_case(&self.metadata.locale) {
            return Err(RecipeError::new(&at, "the default locale's messages are the document's"));
        }
        let shown = self.messages();
        for (key, text) in catalogue {
            if !shown.contains_key(key) {
                return Err(RecipeError::new(
                    &at,
                    format!("{key} is not a message the recipe shows"),
                ));
            }
            check_message(&format!("{at}: {key}"), text)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{check_locale, check_message};

    #[test]
    fn a_message_is_text_with_its_braces_quoted() {
        for ok in ["Maximum players", "It''s here", "Show '{braces}'", "'{'", "a # b", "a < b"] {
            assert!(check_message("f", ok).is_ok(), "{ok}");
        }
        // An apostrophe before a letter quotes nothing, so the brace after it is a placeholder.
        for bad in ["{players}", "Max }", "{n, plural, other {#}}", "''{x}", "don't {"] {
            assert!(check_message("f", bad).is_err(), "{bad}");
        }
        assert!(
            check_message("f", "'{ unclosed").is_ok(),
            "the rest is quoted, as the panel reads it"
        );
        assert!(check_message("f", &"x".repeat(5000)).is_err());
    }

    #[test]
    fn a_locale_is_a_plain_language_tag() {
        for ok in ["en", "tr", "pt-BR", "zh-Hant-TW", "es-419"] {
            assert!(check_locale("l", ok).is_ok(), "{ok}");
        }
        for bad in ["", "e", "english", "en_US", "../en", "en-", "en-toolongsubtag"] {
            assert!(check_locale("l", bad).is_err(), "{bad}");
        }
    }
}
