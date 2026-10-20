// SPDX-License-Identifier: AGPL-3.0-or-later

//! `SessionService` (§9.4 of docs/08-identity.md): a refresh credential exchanged for access
//! tokens and rotated, from the browser's cookie or in the CLI's request body; and a user's
//! sessions listed and ended.

use super::wire::Refuse as _;
use super::{Api, cookie, device, error};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::account::{Exchanged, SessionView};
use korpis_core::audit::failure::Attempt;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::{timestamp, wire_id};

/// Where a refresh credential came from, which is where its successor goes.
enum Carried {
    /// The browser's cookie.
    Cookie(String),
    /// The CLI's request body.
    Body(String),
}

impl Carried {
    fn of(ctx: &RequestContext, body: &str) -> Self {
        if body.is_empty() {
            Self::Cookie(cookie::read(ctx.headers()).unwrap_or_default())
        } else {
            Self::Body(body.to_owned())
        }
    }

    fn secret(&self) -> &str {
        match self {
            Self::Cookie(s) | Self::Body(s) => s,
        }
    }
}

fn exchanged(e: &Exchanged, carried: &Carried) -> ServiceResult<pb::ExchangeResponse> {
    let body = pb::ExchangeResponse {
        access_token: e.access.clone(),
        access_expires_at: Some(timestamp(e.access_expires_at)).into(),
        account: e.holder.account().map(|a| wire_id(IdKind::User, a)).unwrap_or_default(),
        binding: e.holder.binding().map(|b| wire_id(IdKind::Binding, b)).unwrap_or_default(),
        session: wire_id(IdKind::Session, e.session),
        refresh: match carried {
            Carried::Body(_) => e.refresh.expose().to_owned(),
            Carried::Cookie(_) => String::new(),
        },
        session_expires_at: Some(timestamp(e.expires_at)).into(),
        ..Default::default()
    };
    let response = Response::new(body);
    Ok(match carried {
        Carried::Body(_) => response,
        Carried::Cookie(_) => {
            let set =
                cookie::set(e.refresh.expose(), e.expires_at, std::time::SystemTime::now())
                    .ok_or_else(|| error::internal(&"a refresh credential that is not a header"))?;
            response.with_header(http::header::SET_COOKIE, set)
        }
    })
}

fn session_to_pb(v: &SessionView) -> pb::Session {
    let s = &v.session;
    pb::Session {
        id: wire_id(IdKind::Session, s.id),
        kind: s.kind.as_str().to_owned(),
        created_at: Some(timestamp(s.created_at)).into(),
        last_used_at: Some(timestamp(s.last_used_at)).into(),
        expires_at: Some(timestamp(s.expires_at)).into(),
        user_agent: s.user_agent.clone(),
        source: s.source.clone(),
        current: v.current,
        ..Default::default()
    }
}

impl pb::SessionService for Api {
    async fn exchange(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ExchangeRequest>,
    ) -> ServiceResult<pb::ExchangeResponse> {
        let carried = Carried::of(&ctx, request.refresh);
        let ip = ctx.peer_addr().map(|a| a.ip());
        match self.app.exchange(carried.secret()).await {
            Ok(e) => exchanged(&e, &carried),
            Err(e) => Err(self.failed(e, (Attempt::Refresh, ip, None)).await),
        }
    }

    async fn sign_out(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SignOutRequest>,
    ) -> ServiceResult<pb::SignOutResponse> {
        let carried = Carried::of(&ctx, request.refresh);
        let ip = ctx.peer_addr().map(|a| a.ip());
        if let Err(e) = self.app.sign_out(carried.secret()).await {
            return Err(self.failed(e, (Attempt::Refresh, ip, None)).await);
        }
        let response = Response::new(pb::SignOutResponse::default());
        Ok(match carried {
            Carried::Cookie(_) => response.with_header(http::header::SET_COOKIE, cookie::clear()),
            Carried::Body(_) => response,
        })
    }

    async fn list_sessions(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ListSessionsRequest>,
    ) -> ServiceResult<pb::ListSessionsResponse> {
        let caller = self.caller(&ctx).await?;
        let sessions = self.app.sessions(&caller).await.map_err(error::from_app)?;
        Response::ok(pb::ListSessionsResponse {
            sessions: sessions.iter().map(session_to_pb).collect(),
            ..Default::default()
        })
    }

    async fn end_session(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::EndSessionRequest>,
    ) -> ServiceResult<pb::EndSessionResponse> {
        let caller = self.caller(&ctx).await?;
        let id = korpis_wire::id(IdKind::Session, "session", request.session).refuse()?;
        self.app.end_session(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::EndSessionResponse::default())
    }

    async fn end_other_sessions(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::EndOtherSessionsRequest>,
    ) -> ServiceResult<pb::EndOtherSessionsResponse> {
        let caller = self.caller(&ctx).await?;
        let ended = self.app.end_other_sessions(&caller).await.map_err(error::from_app)?;
        Response::ok(pb::EndOtherSessionsResponse { ended, ..Default::default() })
    }

    async fn authorize_device(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::AuthorizeDeviceRequest>,
    ) -> ServiceResult<pb::AuthorizeDeviceResponse> {
        Response::ok(Api::authorize_device(self, &ctx).await?)
    }

    async fn device_token(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DeviceTokenRequest>,
    ) -> ServiceResult<pb::DeviceTokenResponse> {
        Response::ok(Api::device_token(self, &ctx, request.device_code).await?)
    }

    async fn get_device_authorization(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetDeviceAuthorizationRequest>,
    ) -> ServiceResult<pb::GetDeviceAuthorizationResponse> {
        let caller = self.caller(&ctx).await?;
        let a = self
            .app
            .device_authorization(&caller, request.user_code)
            .await
            .map_err(error::from_app)?;
        Response::ok(device::device_authorization(&a))
    }

    async fn approve_device(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ApproveDeviceRequest>,
    ) -> ServiceResult<pb::ApproveDeviceResponse> {
        let caller = self.caller(&ctx).await?;
        let code = request.user_code;
        self.app.decide_device(&caller, code, true).await.map_err(error::from_app)?;
        Response::ok(pb::ApproveDeviceResponse::default())
    }

    async fn deny_device(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::DenyDeviceRequest>,
    ) -> ServiceResult<pb::DenyDeviceResponse> {
        let caller = self.caller(&ctx).await?;
        let code = request.user_code;
        self.app.decide_device(&caller, code, false).await.map_err(error::from_app)?;
        Response::ok(pb::DenyDeviceResponse::default())
    }
}
