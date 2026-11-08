// SPDX-License-Identifier: AGPL-3.0-or-later

//! UI slots: the places in the panel an extension contributes to, and what it contributes. A
//! contribution is declarative, a card showing what one of its actions answers or a form invoking
//! one, rendered by the panel's own components, so it inherits the panel's theming, accessibility
//! and layout and never runs code in the panel's origin: it cannot read the viewer's credential or
//! draw a convincing fake of the panel's own screens. Its action is invoked as any invocation is,
//! authorized for the viewer, so a slot shows nothing its viewer may not do.

use super::{ActionDecl, ArgumentType};

/// A workload's page: the contribution's action takes the workload as its first `workload`
/// argument.
pub const WORKLOAD_PANEL: &str = "workload.panel";

/// An organization's settings: the contribution's action is invoked where the extension is
/// installed.
pub const ORGANIZATION_SETTINGS: &str = "organization.settings";

/// The slots the panel offers, each added for an extension that needed it.
pub const SLOTS: &[&str] = &[WORKLOAD_PANEL, ORGANIZATION_SETTINGS];

/// The longest title.
pub const MAX_TITLE: usize = 128;

/// How a contribution is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Widget {
    /// What its action answers, asked as the slot is shown and again when the viewer asks.
    Card,
    /// A form of its action's arguments, invoking it when the viewer submits.
    Form,
}

impl Widget {
    /// The name a descriptor writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Card => "card",
            Self::Form => "form",
        }
    }
}

/// A contribution, as its descriptor gives it: `{"kind": "card", "action": "players", "title":
/// "Players online"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contribution {
    /// How it is shown.
    pub widget: Widget,
    /// The action it shows or invokes, the part after `ext.<name>.`.
    pub action: String,
    /// What the panel heads it with.
    pub title: String,
}

/// Reads `descriptor`, the contribution to `slot`, against the `actions` the extension declares:
/// a slot the panel offers, a card or a form, an action declared, and for a workload's page one
/// taking a workload.
///
/// # Errors
/// Why it cannot be shown, for the document's author.
pub fn read(slot: &str, descriptor: &str, actions: &[ActionDecl]) -> Result<Contribution, String> {
    if !SLOTS.contains(&slot) {
        return Err(format!("a slot the panel offers: {}", SLOTS.join(", ")));
    }
    if descriptor.trim_start().starts_with("https:") || descriptor.trim_start().starts_with("http:")
    {
        return Err("a declarative card or form: custom frames are not built yet".into());
    }
    let v: serde_json::Value =
        serde_json::from_str(descriptor).map_err(|e| format!("a JSON object: {e}"))?;
    let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or_default();
    let widget = match text("kind") {
        "card" => Widget::Card,
        "form" => Widget::Form,
        _ => return Err("kind: card or form".into()),
    };
    let action = text("action");
    let Some(declared) = actions.iter().find(|a| a.name == action) else {
        return Err(format!("action: one the extension declares, not {action:?}"));
    };
    let takes_workload = declared.arguments.iter().any(|a| a.kind == ArgumentType::Workload);
    if slot == WORKLOAD_PANEL && !takes_workload {
        return Err("action: one taking a workload, the page's".into());
    }
    let title = text("title");
    if title.is_empty() || title.len() > MAX_TITLE || title.chars().any(char::is_control) {
        return Err(format!("title: 1 to {MAX_TITLE} bytes of text"));
    }
    Ok(Contribution { widget, action: action.to_owned(), title: title.to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::ActionArgument;

    fn players() -> Vec<ActionDecl> {
        let server =
            ActionArgument { name: "server".into(), kind: ArgumentType::Workload, required: true };
        vec![
            ActionDecl {
                name: "players".into(),
                description: "who".into(),
                arguments: vec![server],
            },
            ActionDecl { name: "about".into(), description: "what".into(), arguments: vec![] },
        ]
    }

    #[test]
    fn a_contribution_names_an_action_the_slot_can_invoke() {
        let card = r#"{"kind": "card", "action": "players", "title": "Players online"}"#;
        let read_card = read(WORKLOAD_PANEL, card, &players()).unwrap();
        assert_eq!((read_card.widget, read_card.action.as_str()), (Widget::Card, "players"));
        let about = r#"{"kind": "form", "action": "about", "title": "About"}"#;
        assert!(read(ORGANIZATION_SETTINGS, about, &players()).is_ok());
        assert!(read(WORKLOAD_PANEL, about, &players()).is_err(), "it takes no workload");
        assert!(read("workload.sidebar", card, &players()).is_err(), "no such slot");
        let other = r#"{"kind": "card", "action": "kick", "title": "x"}"#;
        assert!(read(WORKLOAD_PANEL, other, &players()).is_err(), "undeclared");
        assert!(read(WORKLOAD_PANEL, "https://ext.example/ui", &players()).is_err());
    }
}
