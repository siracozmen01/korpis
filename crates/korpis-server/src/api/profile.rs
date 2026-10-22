// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ProfileService` (§9.6 of docs/08-identity.md): the signed-in user's own account, its factors
//! read and changed behind a recent sign-in, and the session re-authenticated to make one.

use super::account::{assertion, client, creation, registration};
use super::{Api, cookie, error};
use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::Caller;
use korpis_app::account::{AccountView, Profiled, SecondFactor};
use korpis_core::account::totp::TotpSecret;
use korpis_core::audit::failure::Attempt;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::reauthenticate_with_password_request::Second;
use korpis_wire::binding::binding_to_pb;
use korpis_wire::{timestamp, wire_id};
use std::time::SystemTime;

/// The profile of whom `p` names.
fn profiled(p: Profiled, authenticated_at: Option<SystemTime>) -> pb::GetProfileResponse {
    match p {
        Profiled::Account(v) => profile(v, authenticated_at),
        Profiled::Person(b) => pb::GetProfileResponse {
            person: Some(binding_to_pb(&b)).into(),
            authenticated_at: authenticated_at.map(timestamp).into(),
            ..Default::default()
        },
    }
}

fn profile(v: AccountView, authenticated_at: Option<SystemTime>) -> pb::GetProfileResponse {
    let passkeys = v
        .passkeys
        .into_iter()
        .map(|p| pb::ProfilePasskey {
            credential_id: p.credential_id,
            label: p.label,
            created_at: Some(timestamp(p.created_at)).into(),
            last_used_at: p.last_used_at.map(timestamp).into(),
            backup_eligible: p.backup_eligible,
            ..Default::default()
        })
        .collect();
    pb::GetProfileResponse {
        account: wire_id(IdKind::User, v.account.id),
        name: v.account.name.unwrap_or_default(),
        display_name: v.account.display_name,
        passkeys,
        password: v.factors.password,
        totp: v.factors.totp,
        recovery_codes: v.factors.recovery_codes,
        authenticated_at: authenticated_at.map(timestamp).into(),
        ..Default::default()
    }
}

fn reauthenticated(at: SystemTime) -> ServiceResult<pb::ReauthenticateResponse> {
    Response::ok(pb::ReauthenticateResponse {
        authenticated_at: Some(timestamp(at)).into(),
        ..Default::default()
    })
}

impl Api {
    /// `body`, with a fresh device cookie for the caller's browser: a change to the account's
    /// password or TOTP has just stopped honouring the one it held (§9.6 of
    /// docs/08-identity.md).
    fn with_device<B>(&self, body: B, caller: &Caller) -> ServiceResult<B> {
        let d = self.app.device_cookie(caller).map_err(error::from_app)?;
        let set = cookie::set_device(d.account, &d.cookie, d.expires_at, SystemTime::now())
            .ok_or_else(|| error::internal(&"a device cookie that is not a header value"))?;
        Ok(Response::new(body).with_header(http::header::SET_COOKIE, set))
    }
}

impl pb::ProfileService for Api {
    async fn get_profile(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::GetProfileRequest>,
    ) -> ServiceResult<pb::GetProfileResponse> {
        let caller = self.caller(&ctx).await?;
        let (profile, at) = self.app.profile(&caller).await.map_err(error::from_app)?;
        Response::ok(profiled(profile, at))
    }

    async fn reauthenticate_with_passkey(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SignInWithPasskeyRequest>,
    ) -> ServiceResult<pb::ReauthenticateResponse> {
        let caller = self.caller(&ctx).await?;
        let a = assertion(request.to_owned_message())?;
        let done = self.app.reauthenticate_with_passkey(&caller, a).await;
        let ip = ctx.peer_addr().map(|a| a.ip());
        reauthenticated(self.attempted(done, Attempt::SignIn, ip).await?)
    }

    async fn reauthenticate_with_password(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::ReauthenticateWithPasswordRequest>,
    ) -> ServiceResult<pb::ReauthenticateResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let second = match req.second {
            Some(Second::TotpCode(c)) => SecondFactor::Totp(c),
            Some(Second::RecoveryCode(c)) => SecondFactor::Recovery(c),
            None => return Err(error::invalid("second", "a TOTP code or a recovery code")),
        };
        // The use case charges and records its own failures, as a password sign-in's.
        let done = self
            .app
            .reauthenticate_with_password(&caller, &req.password, second, &client(&ctx))
            .await;
        reauthenticated(done.map_err(super::rate::refused)?)
    }

    async fn begin_add_passkey(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::BeginAddPasskeyRequest>,
    ) -> ServiceResult<pb::BeginAddPasskeyResponse> {
        let caller = self.caller(&ctx).await?;
        let r = self.app.begin_add_passkey(&caller).await.map_err(error::from_app)?;
        Response::ok(pb::BeginAddPasskeyResponse {
            creation: creation(r).into(),
            ..Default::default()
        })
    }

    async fn add_passkey(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::AddPasskeyRequest>,
    ) -> ServiceResult<pb::AddPasskeyResponse> {
        let caller = self.caller(&ctx).await?;
        let req = request.to_owned_message();
        let r = registration(req.registration.as_option())?;
        self.app.add_passkey(&caller, r).await.map_err(error::from_app)?;
        Response::ok(pb::AddPasskeyResponse::default())
    }

    async fn remove_passkey(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RemovePasskeyRequest>,
    ) -> ServiceResult<pb::RemovePasskeyResponse> {
        let caller = self.caller(&ctx).await?;
        let id = request.credential_id;
        self.app.remove_passkey(&caller, id).await.map_err(error::from_app)?;
        Response::ok(pb::RemovePasskeyResponse::default())
    }

    async fn list_ssh_keys(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::ListSshKeysRequest>,
    ) -> ServiceResult<pb::ListSshKeysResponse> {
        super::ssh_keys::list(self, &ctx).await
    }

    async fn add_ssh_key(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::AddSshKeyRequest>,
    ) -> ServiceResult<pb::AddSshKeyResponse> {
        super::ssh_keys::add(self, &ctx, request).await
    }

    async fn remove_ssh_key(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RemoveSshKeyRequest>,
    ) -> ServiceResult<pb::RemoveSshKeyResponse> {
        super::ssh_keys::remove(self, &ctx, request).await
    }

    async fn set_password(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SetPasswordRequest>,
    ) -> ServiceResult<pb::SetPasswordResponse> {
        let caller = self.caller(&ctx).await?;
        self.app.set_password(&caller, request.password).await.map_err(error::from_app)?;
        self.with_device(pb::SetPasswordResponse::default(), &caller)
    }

    async fn remove_password(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::RemovePasswordRequest>,
    ) -> ServiceResult<pb::RemovePasswordResponse> {
        let caller = self.caller(&ctx).await?;
        self.app.remove_password(&caller).await.map_err(error::from_app)?;
        self.with_device(pb::RemovePasswordResponse::default(), &caller)
    }

    async fn enrol_totp(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::EnrolTotpRequest>,
    ) -> ServiceResult<pb::EnrolTotpResponse> {
        let caller = self.caller(&ctx).await?;
        let bytes = <[u8; 20]>::try_from(request.secret)
            .map_err(|_| error::invalid("secret", "the 20 bytes OfferTotp gave"))?;
        let secret = TotpSecret::from_bytes(bytes);
        self.app.enrol_totp(&caller, &secret, request.code).await.map_err(error::from_app)?;
        self.with_device(pb::EnrolTotpResponse::default(), &caller)
    }

    async fn remove_totp(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::RemoveTotpRequest>,
    ) -> ServiceResult<pb::RemoveTotpResponse> {
        let caller = self.caller(&ctx).await?;
        self.app.remove_totp(&caller).await.map_err(error::from_app)?;
        self.with_device(pb::RemoveTotpResponse::default(), &caller)
    }

    async fn renew_recovery_codes(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, pb::RenewRecoveryCodesRequest>,
    ) -> ServiceResult<pb::RenewRecoveryCodesResponse> {
        let caller = self.caller(&ctx).await?;
        let codes = self.app.renew_recovery_codes(&caller).await.map_err(error::from_app)?;
        Response::ok(pb::RenewRecoveryCodesResponse { recovery_codes: codes, ..Default::default() })
    }
}
