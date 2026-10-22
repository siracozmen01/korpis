// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis layout …` (question 2 of §10 of docs/11-surface-web.md): the landing screen an
//! organization composes for those it serves, shown to it and everything beneath it that sets
//! none of its own. `set` reads the layout from a file, in the form `korpis --json layout show`
//! prints its `layout`, so a layout shown, edited, and set again round-trips.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::layout_item::Kind as K;
use std::path::PathBuf;

/// `korpis layout …`.
#[derive(Debug, Clone, Subcommand)]
pub enum LayoutAction {
    /// The layout an organization's landing screen shows, and the organization it comes from.
    Show {
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
    },
    /// Sets an organization's own layout from a JSON file.
    Set {
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
        /// The layout, as `korpis --json layout show` prints its `layout`: a `name` and its
        /// `items`, each one of `places`, `quota`, `notice` (`heading`, `text`), or `link`
        /// (`label`, `url`).
        #[arg(long)]
        from: PathBuf,
    },
    /// Removes an organization's own layout, so its nearest ancestor's applies again.
    Clear {
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
    },
}

fn read(path: &PathBuf) -> Result<pb::Layout, CliError> {
    let at = |e: &dyn std::fmt::Display| CliError::Local(format!("{}: {e}", path.display()));
    let text = std::fs::read_to_string(path).map_err(|e| at(&e))?;
    serde_json::from_str(&text).map_err(|e| at(&e))
}

async fn governing(c: &Connection, org: &str) -> Result<pb::GoverningLayout, CliError> {
    let of =
        pb::Scope { kind: Some(pb::scope::Kind::Organization(org.into())), ..Default::default() };
    let request = pb::GetLayoutRequest { of: of.into(), ..Default::default() };
    let got = pb::OrganizationServiceClient::new(c.http(), c.config()).get_layout(request).await?;
    Ok(got.into_owned().governing.into_option().unwrap_or_default())
}

/// One line for an item.
fn line(i: &pb::LayoutItem) -> String {
    match &i.kind {
        Some(K::Places(_)) => "  what the viewer can reach".to_owned(),
        Some(K::Quota(_)) => "  the quota".to_owned(),
        Some(K::Notice(n)) => format!("  a notice: {}", n.heading),
        Some(K::Link(l)) => format!("  a link: {} ({})", l.label, l.url),
        Some(K::Slot(s)) => format!("  the slot {s}"),
        None => "  (an item this client does not know)".to_owned(),
    }
}

fn said(g: &pb::GoverningLayout) -> Answer {
    let human = match (&g.set_on, g.layout.as_option()) {
        (Some(on), Some(l)) => {
            let name = if l.name.is_empty() { "(the panel's own heading)" } else { &l.name };
            let items = l.items.iter().map(line).collect::<Vec<_>>().join("\n");
            format!("{name}, set on {on}:\n{items}")
        }
        _ => "the panel's own landing: no organization above sets a layout".to_owned(),
    };
    Answer { json: json(g), human }
}

async fn changed(
    c: &Connection,
    org: String,
    layout: Option<pb::Layout>,
) -> Result<Answer, CliError> {
    let version = governing(c, &org).await?.own_version;
    let request = pb::SetLayoutRequest {
        mutation: mutation((version > 0).then_some(version)).into(),
        organization: org,
        layout: layout.into(),
        ..Default::default()
    };
    let orgs = pb::OrganizationServiceClient::new(c.http(), c.config());
    let set = orgs.set_layout(request).await?.into_owned();
    Ok(said(&set.governing.into_option().unwrap_or_default()))
}

/// Runs `a`.
///
/// # Errors
/// The file could not be read or is not a layout, or the control plane refused.
pub async fn layout(c: &Connection, a: &LayoutAction) -> Result<Answer, CliError> {
    match a {
        LayoutAction::Show { organization } => {
            let org = resolve::organization(c, organization.as_deref()).await?;
            Ok(said(&governing(c, &org).await?))
        }
        LayoutAction::Set { organization, from } => {
            let layout = read(from)?;
            let org = resolve::organization(c, organization.as_deref()).await?;
            changed(c, org, Some(layout)).await
        }
        LayoutAction::Clear { organization } => {
            let org = resolve::organization(c, organization.as_deref()).await?;
            changed(c, org, None).await
        }
    }
}
