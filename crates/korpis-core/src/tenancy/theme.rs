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

/// The panel's own colours, as its stylesheet sets them (`web/src/styles.css`): what a token a
/// theme leaves unset is, for the contrast a theme is checked by.
const PANEL: [(&str, &str, &str); 9] = [
    // token, light, dark
    ("background", "#ffffff", "#15171b"),
    ("text", "#1b1d21", "#e7e9ee"),
    ("muted", "#5b6270", "#a3aab8"),
    ("line", "#d5d9e0", "#343944"),
    ("accent", "#1f5fbf", "#7aa7ff"),
    ("accent_text", "#ffffff", "#0b1220"),
    ("danger", "#b3261e", "#ff8a80"),
    ("surface", "#f4f6f9", "#1f2228"),
    ("focus", "#1f5fbf", "#7aa7ff"),
];

/// The pairs of tokens whose contrast is held: text on the background and on a surface, muted
/// text and a danger notice on the background, and text on the accent.
const READ_ON: [(&str, &str); 5] = [
    ("text", "background"),
    ("text", "surface"),
    ("muted", "background"),
    ("danger", "background"),
    ("accent_text", "accent"),
];

/// The least contrast a pair may have: WCAG's for text (success criterion 1.4.3), so a theme
/// cannot make what the panel says unreadable, a warning or a disclosure among it.
pub const MIN_CONTRAST: f64 = 4.5;

/// The relative luminance of `#rrggbb`, as WCAG defines it.
fn luminance(hex: &str) -> f64 {
    let channel = |i: usize| {
        let v = hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).unwrap_or(0);
        let c = f64::from(v) / 255.0;
        if c <= 0.039_28 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
}

/// The contrast ratio of two colours, 1 to 21.
#[must_use]
pub fn contrast(a: &str, b: &str) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Checks that every pair of [`READ_ON`] in `scheme` keeps [`MIN_CONTRAST`], each token unset
/// taken as the panel's own.
fn readable(scheme: &str, set: &BTreeMap<String, Colour>) -> Result<(), ThemeError> {
    let dark = scheme == "dark";
    let colour = |token: &str| -> String {
        let own = PANEL.iter().find(|(t, _, _)| *t == token);
        let panel = own.map_or("#000000", |(_, l, d)| if dark { *d } else { *l });
        set.get(token).map_or_else(|| panel.to_owned(), |c| c.0.clone())
    };
    for (fore, back) in READ_ON {
        let ratio = contrast(&colour(fore), &colour(back));
        if ratio < MIN_CONTRAST {
            let field = format!("{scheme}.{fore}");
            let message = format!(
                "{fore} on {back} has a contrast of {ratio:.1}:1; at least {MIN_CONTRAST}:1 keeps \
                 it readable"
            );
            return Err(refuse(field, message));
        }
    }
    Ok(())
}

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
        let (light, dark) = (colours("light", &self.light)?, colours("dark", &self.dark)?);
        readable("light", &light)?;
        readable("dark", &dark)?;
        Ok(Theme {
            product_name: self.product_name.clone(),
            logo,
            light,
            dark,
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
    fn colours_that_would_hide_what_the_panel_says_are_refused() {
        use super::{MIN_CONTRAST, contrast};
        assert!((contrast("#000000", "#ffffff") - 21.0).abs() < 0.01);
        let mut a = asked();
        a.light = [("text".to_owned(), "#ffffff".to_owned())].into();
        assert_eq!(a.checked().unwrap_err().field, "light.text", "white on the panel's white");
        a.light.insert("background".into(), "#000000".into());
        a.light.insert("surface".into(), "#111111".into());
        a.light.insert("muted".into(), "#bbbbbb".into());
        a.light.insert("danger".into(), "#ff8a80".into());
        assert!(a.checked().is_ok(), "white on black, the rest readable too");
        a.dark = [("accent_text".to_owned(), "#7aa7ff".to_owned())].into();
        let e = a.checked().unwrap_err();
        assert_eq!(e.field, "dark.accent_text", "the accent's own colour on the accent");
        assert!(e.message.contains(&MIN_CONTRAST.to_string()));
    }

    #[test]
    fn the_panels_own_colours_are_those_its_stylesheet_sets() {
        use super::PANEL;
        let css = include_str!("../../../../web/src/styles.css");
        let (light, dark) = css.split_once("prefers-color-scheme: dark").unwrap();
        for (token, l, d) in PANEL {
            let p = match token {
                "background" => "bg",
                "text" => "fg",
                "accent_text" => "accent-fg",
                other => other,
            };
            assert!(light.contains(&format!("--{p}: {l};")), "{token} light");
            assert!(dark.contains(&format!("--{p}: {d};")), "{token} dark");
        }
        assert!(Asked::default().checked().is_ok(), "the panel's own colours are readable");
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
