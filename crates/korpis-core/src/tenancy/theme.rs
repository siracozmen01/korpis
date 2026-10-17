// SPDX-License-Identifier: AGPL-3.0-or-later

//! An organization's brand (§7 of docs/11-surface-web.md): data the panel applies, never a
//! patched file. Every field is optional, an unset one keeping the panel's own, and every field
//! is bounded and checked here, so what the panel is given is a name, an image, a fixed set of
//! colours, some text, and a stylesheet that imports nothing.

use std::collections::BTreeMap;

/// The longest product name, in characters.
pub const MAX_PRODUCT_NAME: usize = 64;
/// The longest sign-in text, in characters.
pub const MAX_SIGN_IN_TEXT: usize = 2_000;
/// The largest stylesheet, in bytes.
pub const MAX_STYLESHEET: usize = 64 << 10;
/// The largest logo, in bytes.
pub const MAX_LOGO: usize = 256 << 10;

/// The colour tokens a theme may set: the panel's own, by the names its stylesheet uses.
pub const TOKENS: &[&str] =
    &["background", "text", "muted", "line", "accent", "accent_text", "danger", "surface", "focus"];

/// What a logo is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoKind {
    /// A PNG image.
    Png,
    /// A WebP image.
    Webp,
    /// An SVG image, which the panel shows as an image, where it runs nothing.
    Svg,
}

impl LogoKind {
    /// The media type it is served as.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Svg => "image/svg+xml",
        }
    }

    fn of(media_type: &str) -> Option<Self> {
        [Self::Png, Self::Webp, Self::Svg].into_iter().find(|k| k.media_type() == media_type)
    }

    /// Whether `data` begins as this kind of image does.
    fn holds(self, data: &[u8]) -> bool {
        match self {
            Self::Png => data.starts_with(b"\x89PNG\r\n\x1a\n"),
            Self::Webp => data.get(..4) == Some(b"RIFF") && data.get(8..12) == Some(b"WEBP"),
            Self::Svg => {
                let text = std::str::from_utf8(data).unwrap_or_default().trim_start();
                text.starts_with("<svg") || text.starts_with("<?xml")
            }
        }
    }
}

/// A logo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logo {
    /// What it is.
    pub kind: LogoKind,
    /// Its bytes.
    pub data: Vec<u8>,
}

/// A colour, `#rrggbb`, kept lower case.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Colour(String);

impl Colour {
    /// The colour `#rrggbb`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A theme, every field checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Theme {
    /// The name the panel shows for itself; empty for its own.
    pub product_name: String,
    /// The logo; none for the panel's own.
    pub logo: Option<Logo>,
    /// The colours for the light scheme, by token.
    pub light: BTreeMap<String, Colour>,
    /// The colours for the dark scheme, by token.
    pub dark: BTreeMap<String, Colour>,
    /// What the sign-in page says above its form.
    pub sign_in_text: String,
    /// A stylesheet applied after the panel's own.
    pub stylesheet: String,
}

/// A theme as it was asked for, before it is checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Asked {
    /// As [`Theme::product_name`].
    pub product_name: String,
    /// The logo's media type and bytes.
    pub logo: Option<(String, Vec<u8>)>,
    /// The light scheme's colours, by token.
    pub light: BTreeMap<String, String>,
    /// The dark scheme's colours, by token.
    pub dark: BTreeMap<String, String>,
    /// As [`Theme::sign_in_text`].
    pub sign_in_text: String,
    /// As [`Theme::stylesheet`].
    pub stylesheet: String,
}

/// Why a theme is refused: the field, and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct ThemeError {
    /// The field, as the wire names it.
    pub field: String,
    /// What is wrong with it.
    pub message: String,
}

fn refuse(field: impl Into<String>, message: impl Into<String>) -> ThemeError {
    ThemeError { field: field.into(), message: message.into() }
}

fn colours(
    scheme: &str,
    asked: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, Colour>, ThemeError> {
    let mut checked = BTreeMap::new();
    for (token, value) in asked {
        let field = format!("{scheme}.{token}");
        if !TOKENS.contains(&token.as_str()) {
            return Err(refuse(field, "not a colour the panel has"));
        }
        let hex = value.strip_prefix('#').filter(|h| h.len() == 6);
        if !hex.is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit())) {
            return Err(refuse(field, "a colour is #rrggbb"));
        }
        checked.insert(token.clone(), Colour(value.to_ascii_lowercase()));
    }
    Ok(checked)
}

/// Whether `text` holds an `@import`, in any case and however it is escaped into CSS: a
/// backslash escape is refused outright, since a stylesheet that needs one to say `@import` is
/// one the panel does not take.
fn imports(text: &str) -> bool {
    text.contains('\\') || text.to_ascii_lowercase().contains("@import")
}

impl Asked {
    /// The theme, every field within its bounds.
    ///
    /// # Errors
    /// A field is out of bounds, a colour is not one the panel has or not `#rrggbb`, the logo is
    /// not the image its media type says, or the stylesheet imports.
    pub fn checked(&self) -> Result<Theme, ThemeError> {
        let line = |s: &str| s.chars().any(char::is_control);
        if self.product_name.chars().count() > MAX_PRODUCT_NAME || line(&self.product_name) {
            let message = format!("at most {MAX_PRODUCT_NAME} characters, on one line");
            return Err(refuse("product_name", message));
        }
        let text_ok = |c: char| !c.is_control() || c == '\n';
        if self.sign_in_text.chars().count() > MAX_SIGN_IN_TEXT
            || !self.sign_in_text.chars().all(text_ok)
        {
            let message = format!("at most {MAX_SIGN_IN_TEXT} characters, no control characters");
            return Err(refuse("sign_in_text", message));
        }
        if self.stylesheet.len() > MAX_STYLESHEET || imports(&self.stylesheet) {
            let message =
                format!("at most {} KiB, with no @import or escape", MAX_STYLESHEET >> 10);
            return Err(refuse("stylesheet", message));
        }
        let logo = match &self.logo {
            None => None,
            Some((media_type, data)) => {
                let kind = LogoKind::of(media_type).ok_or_else(|| {
                    refuse("logo.media_type", "image/png, image/webp, or image/svg+xml")
                })?;
                if data.len() > MAX_LOGO || !kind.holds(data) {
                    let message = format!("a {media_type} of at most {} KiB", MAX_LOGO >> 10);
                    return Err(refuse("logo.data", message));
                }
                Some(Logo { kind, data: data.clone() })
            }
        };
        Ok(Theme {
            product_name: self.product_name.clone(),
            logo,
            light: colours("light", &self.light)?,
            dark: colours("dark", &self.dark)?,
            sign_in_text: self.sign_in_text.clone(),
            stylesheet: self.stylesheet.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::{Asked, MAX_STYLESHEET};

    fn asked() -> Asked {
        Asked {
            product_name: "Acme Hosting".into(),
            logo: Some((
                "image/svg+xml".into(),
                b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(),
            )),
            light: [("accent".to_owned(), "#1F5FBF".to_owned())].into(),
            sign_in_text: "Welcome back.\nSign in to manage your servers.".into(),
            stylesheet: ".brand { font-weight: 600 }".into(),
            ..Asked::default()
        }
    }

    #[test]
    fn a_theme_within_its_bounds_is_taken_its_colours_kept_lower_case() {
        let t = asked().checked().unwrap();
        assert_eq!(t.light["accent"].as_str(), "#1f5fbf");
        assert!(
            Asked::default().checked().unwrap().logo.is_none(),
            "nothing set is the panel's own"
        );
    }

    #[test]
    fn what_is_out_of_bounds_is_refused_by_its_field() {
        let field = |a: Asked| a.checked().unwrap_err().field;
        let with = |f: &dyn Fn(&mut Asked)| {
            let mut a = asked();
            f(&mut a);
            a
        };
        assert_eq!(
            field(with(&|a| a.light = [("shadow".into(), "#000000".into())].into())),
            "light.shadow"
        );
        assert_eq!(field(with(&|a| a.dark = [("text".into(), "red".into())].into())), "dark.text");
        assert_eq!(field(with(&|a| a.product_name = "two\nlines".into())), "product_name");
        assert_eq!(field(with(&|a| a.stylesheet = "@IMPORT url(x.css);".into())), "stylesheet");
        assert_eq!(field(with(&|a| a.stylesheet = "@\\69mport url(x.css);".into())), "stylesheet");
        assert_eq!(field(with(&|a| a.stylesheet = "a{}".repeat(MAX_STYLESHEET))), "stylesheet");
        assert_eq!(
            field(with(&|a| a.logo = Some(("image/gif".into(), vec![1])))),
            "logo.media_type"
        );
        assert_eq!(
            field(with(&|a| a.logo = Some(("image/png".into(), b"<svg/>".to_vec())))),
            "logo.data",
            "an image that is not what its media type says"
        );
    }
}
