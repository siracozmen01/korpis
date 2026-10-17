// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis theme …` (§7 of docs/11-surface-web.md): an organization's brand, which the panel
//! wears for it and everything beneath it that sets none of its own. `set` replaces the
//! organization's own theme with what its flags say, each unsaid field the panel's own.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};

/// `korpis theme …`.
#[derive(Debug, Clone, Subcommand)]
pub enum ThemeAction {
    /// The theme an organization wears, and the organization it comes from.
    Show {
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
    },
    /// Sets an organization's own theme.
    Set {
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
        /// The name the panel shows for itself.
        #[arg(long)]
        product_name: Option<String>,
        /// A logo: a `.png`, `.webp`, or `.svg` file of at most 256 KiB.
        #[arg(long)]
        logo: Option<PathBuf>,
        /// What the sign-in page says above its form.
        #[arg(long)]
        sign_in_text: Option<String>,
        /// A stylesheet applied after the panel's own, at most 64 KiB, without `@import`.
        #[arg(long)]
        stylesheet: Option<PathBuf>,
        /// A colour of the light scheme, `TOKEN=#rrggbb`: background, text, muted, line,
        /// accent, `accent_text`, danger, surface, or focus. Repeatable.
        #[arg(long)]
        light: Vec<String>,
        /// A colour of the dark scheme, as `--light`. Repeatable.
        #[arg(long)]
        dark: Vec<String>,
    },
    /// Removes an organization's own theme, so its nearest ancestor's applies again.
    Clear {
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
    },
}

fn local(path: &Path, e: &std::io::Error) -> CliError {
    CliError::Local(format!("{}: {e}", path.display()))
}

/// `TOKEN=#rrggbb` pairs as the scheme's colours; the control plane checks the tokens and values.
fn colours(flag: &str, pairs: &[String]) -> Result<pb::ThemeColors, CliError> {
    let mut c = pb::ThemeColors::default();
    for pair in pairs {
        let (token, value) = pair
            .split_once('=')
            .ok_or_else(|| CliError::Local(format!("--{flag} {pair}: TOKEN=#rrggbb")))?;
        let slot = match token {
            "background" => &mut c.background,
            "text" => &mut c.text,
            "muted" => &mut c.muted,
            "line" => &mut c.line,
            "accent" => &mut c.accent,
            "accent_text" => &mut c.accent_text,
            "danger" => &mut c.danger,
            "surface" => &mut c.surface,
            "focus" => &mut c.focus,
            other => return Err(CliError::Local(format!("--{flag}: no colour named {other}"))),
        };
        value.clone_into(slot);
    }
    Ok(c)
}

fn logo(path: &Path) -> Result<pb::Logo, CliError> {
    let media_type = match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        _ => return Err(CliError::Local(format!("{}: a .png, .webp, or .svg", path.display()))),
    };
    let data = std::fs::read(path).map_err(|e| local(path, &e))?;
    Ok(pb::Logo { media_type: media_type.into(), data, ..Default::default() })
}

async fn governing(c: &Connection, org: &str) -> Result<pb::GoverningTheme, CliError> {
    let of =
        pb::Scope { kind: Some(pb::scope::Kind::Organization(org.into())), ..Default::default() };
    let request = pb::GetThemeRequest { of: of.into(), ..Default::default() };
    let got = pb::OrganizationServiceClient::new(c.http(), c.config()).get_theme(request).await?;
    Ok(got.into_owned().governing.into_option().unwrap_or_default())
}

fn said(g: &pb::GoverningTheme) -> Answer {
    let human = match (&g.set_on, g.theme.as_option()) {
        (Some(on), Some(t)) => {
            let name =
                if t.product_name.is_empty() { "(the panel's own name)" } else { &t.product_name };
            let logo = t
                .logo
                .as_option()
                .map_or("no logo".to_owned(), |l| format!("a {} logo", l.media_type));
            format!("{name}, {logo}, {} bytes of stylesheet, set on {on}", t.stylesheet.len())
        }
        _ => "the panel's own theme: no organization above sets one".to_owned(),
    };
    Answer { json: json(g), human }
}

/// Runs `a`.
///
/// # Errors
/// A local file could not be read, or the control plane refused.
pub async fn theme(c: &Connection, a: &ThemeAction) -> Result<Answer, CliError> {
    let orgs = pb::OrganizationServiceClient::new(c.http(), c.config());
    match a {
        ThemeAction::Show { organization } => {
            let org = resolve::organization(c, organization.as_deref()).await?;
            Ok(said(&governing(c, &org).await?))
        }
        ThemeAction::Set {
            organization,
            product_name,
            logo: l,
            sign_in_text,
            stylesheet,
            light,
            dark,
        } => {
            let org = resolve::organization(c, organization.as_deref()).await?;
            let stylesheet = match stylesheet {
                Some(p) => std::fs::read_to_string(p).map_err(|e| local(p, &e))?,
                None => String::new(),
            };
            let theme = pb::Theme {
                product_name: product_name.clone().unwrap_or_default(),
                logo: l.as_deref().map(logo).transpose()?.into(),
                light: colours("light", light)?.into(),
                dark: colours("dark", dark)?.into(),
                sign_in_text: sign_in_text.clone().unwrap_or_default(),
                stylesheet,
                ..Default::default()
            };
            let version = governing(c, &org).await?.own_version;
            let request = pb::SetThemeRequest {
                mutation: mutation((version > 0).then_some(version)).into(),
                organization: org,
                theme: theme.into(),
                ..Default::default()
            };
            let set = orgs.set_theme(request).await?.into_owned();
            Ok(said(&set.governing.into_option().unwrap_or_default()))
        }
        ThemeAction::Clear { organization } => {
            let org = resolve::organization(c, organization.as_deref()).await?;
            let version = governing(c, &org).await?.own_version;
            let request = pb::SetThemeRequest {
                mutation: mutation((version > 0).then_some(version)).into(),
                organization: org,
                ..Default::default()
            };
            let set = orgs.set_theme(request).await?.into_owned();
            Ok(said(&set.governing.into_option().unwrap_or_default()))
        }
    }
}
