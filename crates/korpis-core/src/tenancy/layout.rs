// SPDX-License-Identifier: AGPL-3.0-or-later

//! A landing screen an organization composes for those it serves (question 2 of §10 of
//! docs/11-surface-web.md): data, an ordered arrangement of the panel's own components, each of
//! which renders under the viewer's own authority. A layout places only what exists, so an
//! extension's slot is refused until extensions declare slots (§5 of docs/16-extensions.md), and
//! it always places what the viewer's grants open, so a host cannot hide a customer's access.

/// The longest name, in characters.
pub const MAX_NAME: usize = 64;
/// The most items a layout holds.
pub const MAX_ITEMS: usize = 24;
/// The longest notice heading, in characters.
pub const MAX_HEADING: usize = 120;
/// The longest notice text, in characters.
pub const MAX_TEXT: usize = 2_000;
/// The longest link label, in characters.
pub const MAX_LABEL: usize = 64;
/// The longest link address, in bytes.
pub const MAX_URL: usize = 2_048;

/// One component of a landing screen, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// What the viewer's grants open.
    Places,
    /// The quota of the viewer's top organization, as the viewer reads it.
    Quota,
    /// Words from whoever composed the layout, shown as text.
    Notice {
        /// A heading, on one line.
        heading: String,
        /// The text, its line breaks kept.
        text: String,
    },
    /// A link elsewhere, shown with the host it goes to.
    Link {
        /// What it says.
        label: String,
        /// Where it goes: an `https` address with a host and no user information.
        url: String,
    },
}

/// One component as it was asked for, before it is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskedItem {
    /// As [`Item::Places`].
    Places,
    /// As [`Item::Quota`].
    Quota,
    /// As [`Item::Notice`].
    Notice {
        /// The heading.
        heading: String,
        /// The text.
        text: String,
    },
    /// As [`Item::Link`].
    Link {
        /// The label.
        label: String,
        /// The address.
        url: String,
    },
    /// An extension's contribution to a declared slot, by the slot's name.
    Slot(String),
}

/// A layout, every item checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The landing screen's heading; empty for the panel's own.
    pub name: String,
    /// Its components, in order; [`Item::Places`] exactly once.
    pub items: Vec<Item>,
}

/// A layout as it was asked for, before it is checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Asked {
    /// As [`Layout::name`].
    pub name: String,
    /// As [`Layout::items`].
    pub items: Vec<AskedItem>,
}

/// Why a layout is refused: the field, and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct LayoutError {
    /// The field, as the wire names it.
    pub field: String,
    /// What is wrong with it.
    pub message: String,
}

fn refuse(field: impl Into<String>, message: impl Into<String>) -> LayoutError {
    LayoutError { field: field.into(), message: message.into() }
}

/// Whether `s` is at most `max` characters on one line.
fn one_line(s: &str, max: usize) -> bool {
    s.chars().count() <= max && !s.chars().any(char::is_control)
}

/// The host `url` goes to, if it is an `https` address the panel may link to: a host, no user
/// information (which would let `https://bank.example@elsewhere.example` read as the bank), and
/// nothing a browser would read past.
fn link_host(url: &str) -> Option<&str> {
    if url.len() > MAX_URL || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once(':').map_or(authority, |(h, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) { h } else { authority }
    });
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-');
    let address = host.strip_prefix('[').and_then(|h| h.strip_suffix(']'));
    let bracketed = address.is_some_and(|a| a.chars().all(|c| c.is_ascii_hexdigit() || c == ':'));
    (!host.is_empty() && (host.chars().all(allowed) || bracketed)).then_some(host)
}

fn item(i: usize, asked: &AskedItem) -> Result<Item, LayoutError> {
    let at = |f: &str| format!("items[{i}].{f}");
    match asked {
        AskedItem::Places => Ok(Item::Places),
        AskedItem::Quota => Ok(Item::Quota),
        AskedItem::Notice { heading, text } => {
            if heading.is_empty() || !one_line(heading, MAX_HEADING) {
                let message = format!("1 to {MAX_HEADING} characters, on one line");
                return Err(refuse(at("notice.heading"), message));
            }
            let text_ok = |c: char| !c.is_control() || c == '\n';
            if text.chars().count() > MAX_TEXT || !text.chars().all(text_ok) {
                let message = format!("at most {MAX_TEXT} characters, no control characters");
                return Err(refuse(at("notice.text"), message));
            }
            Ok(Item::Notice { heading: heading.clone(), text: text.clone() })
        }
        AskedItem::Link { label, url } => {
            if label.is_empty() || !one_line(label, MAX_LABEL) {
                let message = format!("1 to {MAX_LABEL} characters, on one line");
                return Err(refuse(at("link.label"), message));
            }
            if link_host(url).is_none() {
                let message = format!(
                    "an https address with a host and no user information, at most {MAX_URL} \
                     bytes"
                );
                return Err(refuse(at("link.url"), message));
            }
            Ok(Item::Link { label: label.clone(), url: url.clone() })
        }
        AskedItem::Slot(name) => Err(refuse(
            at("slot"),
            format!(
                "no extension declares a slot named {name:?}: a layout places only what exists, \
                 and extensions' slots are not yet built"
            ),
        )),
    }
}

impl Asked {
    /// The layout, every item within its bounds.
    ///
    /// # Errors
    /// The name or an item is out of bounds, a link is not an `https` address the panel may
    /// link to, a slot is named that no extension declares, there are more than [`MAX_ITEMS`]
    /// items, or what the viewer's grants open is placed other than exactly once, or the quota
    /// more than once.
    pub fn checked(&self) -> Result<Layout, LayoutError> {
        if !one_line(&self.name, MAX_NAME) {
            let message = format!("at most {MAX_NAME} characters, on one line");
            return Err(refuse("name", message));
        }
        if self.items.len() > MAX_ITEMS {
            return Err(refuse("items", format!("at most {MAX_ITEMS} items")));
        }
        let items = self.items.iter().enumerate().map(|(i, a)| item(i, a));
        let items = items.collect::<Result<Vec<_>, _>>()?;
        if items.iter().filter(|i| **i == Item::Places).count() != 1 {
            let message = "what the viewer's grants open is placed exactly once: a layout \
                           arranges a landing screen and never hides what a viewer can reach";
            return Err(refuse("items", message));
        }
        if items.iter().filter(|i| **i == Item::Quota).count() > 1 {
            return Err(refuse("items", "the quota is placed at most once"));
        }
        Ok(Layout { name: self.name.clone(), items })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    fn notice(heading: &str, text: &str) -> AskedItem {
        AskedItem::Notice { heading: heading.into(), text: text.into() }
    }

    fn link(url: &str) -> AskedItem {
        AskedItem::Link { label: "Support".into(), url: url.into() }
    }

    fn asked(items: Vec<AskedItem>) -> Asked {
        Asked { name: "Pinecone".into(), items }
    }

    fn refused(items: Vec<AskedItem>) -> String {
        asked(items).checked().unwrap_err().field
    }

    #[test]
    fn a_layout_within_its_bounds_is_taken_in_its_order() {
        let items = vec![
            notice("Maintenance tonight", "From 22:00.\nAbout an hour."),
            AskedItem::Places,
            link("https://help.pinecone.example/desk?from=panel"),
            AskedItem::Quota,
        ];
        let layout = asked(items).checked().unwrap();
        assert_eq!(layout.name, "Pinecone");
        assert_eq!(layout.items.len(), 4);
        assert_eq!(layout.items[1], Item::Places);
    }

    #[test]
    fn what_the_viewer_reaches_is_placed_exactly_once() {
        assert_eq!(refused(vec![notice("Hello", "")]), "items");
        assert_eq!(refused(vec![AskedItem::Places, AskedItem::Places]), "items");
        let twice = vec![AskedItem::Places, AskedItem::Quota, AskedItem::Quota];
        assert_eq!(refused(twice), "items");
    }

    #[test]
    fn a_slot_no_extension_declares_is_refused() {
        let slot = AskedItem::Slot("ext.backup.status".into());
        assert_eq!(refused(vec![AskedItem::Places, slot]), "items[1].slot");
    }

    #[test]
    fn a_link_goes_only_to_an_https_host_it_shows() {
        assert_eq!(link_host("https://help.pinecone.example/a"), Some("help.pinecone.example"));
        assert_eq!(link_host("https://help.pinecone.example:8443"), Some("help.pinecone.example"));
        assert_eq!(link_host("https://[2001:db8::1]/"), Some("[2001:db8::1]"));
        for bad in [
            "http://help.pinecone.example/",
            "javascript:alert(1)",
            "https://bank.example@elsewhere.example/",
            "https:///nothing",
            "https://help.pinecone.example/a b",
        ] {
            assert_eq!(link_host(bad), None, "{bad}");
            assert_eq!(refused(vec![AskedItem::Places, link(bad)]), "items[1].link.url", "{bad}");
        }
    }

    #[test]
    fn what_is_out_of_bounds_is_refused_by_its_field() {
        let long = "x".repeat(MAX_HEADING + 1);
        assert_eq!(refused(vec![AskedItem::Places, notice(&long, "")]), "items[1].notice.heading");
        let text = "x".repeat(MAX_TEXT + 1);
        assert_eq!(refused(vec![notice("Hi", &text), AskedItem::Places]), "items[0].notice.text");
        let many = vec![AskedItem::Places; MAX_ITEMS + 1];
        assert_eq!(refused(many), "items");
        let named = Asked { name: "two\nlines".into(), items: vec![AskedItem::Places] };
        assert_eq!(named.checked().unwrap_err().field, "name");
    }
}
