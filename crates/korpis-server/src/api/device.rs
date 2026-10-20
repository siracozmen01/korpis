// SPDX-License-Identifier: AGPL-3.0-or-later

//! The CLI's sign-in, the device authorization grant of RFC 8628 (§9.4 of
//! docs/08-identity.md), as `SessionService` answers it: what each procedure sends back, and
//! where the user code is typed.

use super::account::client;
use super::{Api, error};
use connectrpc::{ConnectError, RequestContext};
use korpis_app::account::{Authorized, DeviceSignedIn};
use korpis_core::account::device_grant::{DeviceAuthorization, shown};
use korpis_core::account::session::SessionKind;
use korpis_core::audit::failure::Attempt;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::{timestamp, wire_id};

/// The panel's page a user code is typed into.
const DEVICE_PAGE: &str = "/device";

impl Api {
    /// Begins a device authorization for the CLI calling with `ctx`.
    pub(super) async fn authorize_device(
        &self,
        ctx: &RequestContext,
    ) -> Result<pb::AuthorizeDeviceResponse, ConnectError> {
        let page =
            self.app.panel_origin().map(|o| format!("{o}{DEVICE_PAGE}")).ok_or_else(|| {
                error::from_app(korpis_app::AppError::Unavailable {
                    what: "the CLI's sign-in",
                    detail: "no --public-url is configured, so there is no panel to approve it in"
                        .into(),
                })
            })?;
        let cli = korpis_app::account::Client { kind: SessionKind::Cli, ..client(ctx) };
        let a = self.app.authorize_device(&cli).await.map_err(error::from_app)?;
        Ok(authorized(a, &page))
    }

    /// Polls with `device_code`, from the CLI calling with `ctx`.
    pub(super) async fn device_token(
        &self,
        ctx: &RequestContext,
        device_code: &str,
    ) -> Result<pb::DeviceTokenResponse, ConnectError> {
        let cli = korpis_app::account::Client { kind: SessionKind::Cli, ..client(ctx) };
        let ip = cli.ip;
        match self.app.device_token(device_code, &cli).await {
            Ok(signed_in) => Ok(device_signed_in(&signed_in)),
            Err(e @ korpis_app::AppError::Unauthenticated) => {
                Err(self.failed(e, (Attempt::Refresh, ip, None)).await)
            }
            Err(e) => Err(error::from_app(e)),
        }
    }
}

fn authorized(a: Authorized, page: &str) -> pb::AuthorizeDeviceResponse {
    pb::AuthorizeDeviceResponse {
        device_code: a.device_code.expose().to_owned(),
        verification_uri_complete: format!("{page}?code={}", a.user_code),
        verification_uri: page.to_owned(),
        user_code: a.user_code,
        expires_at: Some(timestamp(a.expires_at)).into(),
        interval: u32::try_from(a.interval.as_secs()).unwrap_or(u32::MAX),
        ..Default::default()
    }
}

fn device_signed_in(s: &DeviceSignedIn) -> pb::DeviceTokenResponse {
    pb::DeviceTokenResponse {
        account: s.holder.account().map(|a| wire_id(IdKind::User, a)).unwrap_or_default(),
        binding: s.holder.binding().map(|b| wire_id(IdKind::Binding, b)).unwrap_or_default(),
        session: wire_id(IdKind::Session, s.session.id),
        refresh: s.session.refresh.expose().to_owned(),
        session_expires_at: Some(timestamp(s.session.expires_at)).into(),
        ..Default::default()
    }
}

/// A pending device authorization, as the panel shows it before its user decides.
pub(super) fn device_authorization(a: &DeviceAuthorization) -> pb::GetDeviceAuthorizationResponse {
    pb::GetDeviceAuthorizationResponse {
        user_code: shown(&a.user_code),
        created_at: Some(timestamp(a.created_at)).into(),
        expires_at: Some(timestamp(a.expires_at)).into(),
        user_agent: a.user_agent.clone(),
        source: a.source.clone(),
        ..Default::default()
    }
}
