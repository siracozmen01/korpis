// SPDX-License-Identifier: AGPL-3.0-or-later

//! `OrganizationService.GetTheme`, `GetLandingTheme`, and `SetTheme`, and
//! `ServerInfoService.GetSignInTheme` (§7 of docs/11-surface-web.md).

use super::wire::{self, Refuse};
use super::{Api, encode, error, mutation_key, respond};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Idempotent;
use korpis_app::tenancy::{GoverningTheme, SetTheme};
use korpis_core::id::IdKind;
use korpis_core::tenancy::theme::{Asked, Theme};
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;

fn colours_to_pb(c: &BTreeMap<String, korpis_core::tenancy::theme::Colour>) -> pb::ThemeColors {
    let get = |k: &str| c.get(k).map(|v| v.as_str().to_owned()).unwrap_or_default();
    pb::ThemeColors {
        background: get("background"),
        text: get("text"),
        muted: get("muted"),
        line: get("line"),
        accent: get("accent"),
        accent_text: get("accent_text"),
        danger: get("danger"),
        surface: get("surface"),
        focus: get("focus"),
        ..Default::default()
    }
}

fn colours_from_pb(c: Option<&pb::ThemeColors>) -> BTreeMap<String, String> {
    let Some(c) = c else { return BTreeMap::new() };
    [
        ("background", &c.background),
        ("text", &c.text),
        ("muted", &c.muted),
        ("line", &c.line),
        ("accent", &c.accent),
        ("accent_text", &c.accent_text),
        ("danger", &c.danger),
        ("surface", &c.surface),
        ("focus", &c.focus),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .map(|(k, v)| (k.to_owned(), v.clone()))
    .collect()
}

fn theme_to_pb(t: &Theme) -> pb::Theme {
    pb::Theme {
        product_name: t.product_name.clone(),
        logo: t
            .logo
            .as_ref()
            .map(|l| pb::Logo {
                media_type: l.kind.media_type().to_owned(),
                data: l.data.clone(),
                ..Default::default()
            })
            .into(),
        light: colours_to_pb(&t.light).into(),
        dark: colours_to_pb(&t.dark).into(),
        sign_in_text: t.sign_in_text.clone(),
        stylesheet: t.stylesheet.clone(),
        ..Default::default()
    }
}

/// A theme as the wire asked for it, to be checked by the application.
fn asked_from_pb(t: &pb::Theme) -> Asked {
    Asked {
        product_name: t.product_name.clone(),
        logo: t.logo.as_option().map(|l| (l.media_type.clone(), l.data.clone())),
        light: colours_from_pb(t.light.as_option()),
        dark: colours_from_pb(t.dark.as_option()),
        sign_in_text: t.sign_in_text.clone(),
        stylesheet: t.stylesheet.clone(),
    }
}

fn governing_to_pb(g: &GoverningTheme) -> pb::GoverningTheme {
    let set = g.set.as_ref();
    pb::GoverningTheme {
        theme: set.map(|s| theme_to_pb(&s.theme)).into(),
        set_on: set.map(|s| wire::wire_id(IdKind::Organization, s.organization)),
        own_version: g.own_version,
        ..Default::default()
    }
}

fn set_response(g: &GoverningTheme) -> pb::SetThemeResponse {
    pb::SetThemeResponse { governing: governing_to_pb(g).into(), ..Default::default() }
}

/// The theme that applies to the object.
pub(super) async fn get(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::GetThemeRequest>,
) -> ServiceResult<pb::GetThemeResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let of = req.of.as_option().cloned().unwrap_or_default();
    let object = korpis_wire::scope::object_from_pb("of", &of).refuse()?;
    let g = api.app.get_theme(&caller, object).await.map_err(error::from_app)?;
    Response::ok(pb::GetThemeResponse {
        governing: governing_to_pb(&g).into(),
        ..Default::default()
    })
}

/// Sets or removes the organization's own theme.
pub(super) async fn set(
    api: &Api,
    ctx: &RequestContext,
    request: ServiceRequest<'_, pb::SetThemeRequest>,
) -> ServiceResult<pb::SetThemeResponse> {
    let caller = api.caller(ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let expected_version = req.mutation.as_option().and_then(|m| m.expected_version).unwrap_or(0);
    let cmd = SetTheme {
        organization: wire::id(IdKind::Organization, "organization", &req.organization).refuse()?,
        theme: req.theme.as_option().map(asked_from_pb),
        expected_version,
    };
    let encoder = |g: &GoverningTheme| encode(&set_response(g));
    let idem = Idempotent { key, encode: &encoder };
    let reply = api.app.set_theme(&caller, &idem, cmd).await.map_err(error::from_app)?;
    respond(reply, set_response)
}

/// The theme the sign-in page wears before anyone has signed in, at `portal` or the
/// installation's own address.
pub(super) async fn sign_in(
    api: &Api,
    portal: Option<uuid::Uuid>,
) -> ServiceResult<pb::GetSignInThemeResponse> {
    let g = api.app.sign_in_theme(portal).await.map_err(error::from_app)?;
    Response::ok(pb::GetSignInThemeResponse {
        governing: governing_to_pb(&g).into(),
        ..Default::default()
    })
}

/// The theme a signed-in caller wears: that of the organization they land at.
pub(super) async fn landing(
    api: &Api,
    ctx: &RequestContext,
) -> ServiceResult<pb::GetLandingThemeResponse> {
    let caller = api.caller(ctx).await?;
    let portal = super::account::at(ctx).map(|p| p.portal);
    let g = api.app.landing_theme(&caller, portal).await.map_err(error::from_app)?;
    Response::ok(pb::GetLandingThemeResponse {
        governing: governing_to_pb(&g).into(),
        ..Default::default()
    })
}
