// SPDX-License-Identifier: AGPL-3.0-or-later

//! `AccountService` (§9.6 of docs/08-identity.md): redeeming an invitation and signing in. No
//! procedure here takes a credential in its headers; what each checks is in its request, and each
//! refused is a failed attempt charged to its source (§10 of docs/10-api.md), so a source that
//! keeps guessing is answered `RATE_LIMITED` instead.

use super::{Api, error};
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::AppError;
use korpis_app::account::{
    Assertion, NewRegistration, PasswordRedemption, Redeemed, Registration, SecondFactor,
};
use korpis_core::account::totp::TotpSecret;
use korpis_core::audit::failure::Attempt;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::sign_in_with_password_request::Second;
use korpis_wire::{timestamp, wire_id};
use std::net::IpAddr;
use uuid::Uuid;
use zeroize::Zeroizing;

/// The issuer an authenticator app shows beside a code, when the installation has no site.
const ISSUER: &str = "Korpis";

fn creation(r: Registration) -> pb::PasskeyCreation {
    pb::PasskeyCreation {
        challenge_id: r.challenge_id.to_string(),
        challenge: r.challenge.to_vec(),
        rp_id: r.rp_id,
        user_id: r.user_id.as_bytes().to_vec(),
        user_name: r.user_name,
        display_name: r.display_name,
        exclude_credentials: r.exclude,
        ..Default::default()
    }
}

fn challenge_id(field: &str, s: &str) -> Result<Uuid, ConnectError> {
    Uuid::parse_str(s).map_err(|_| error::invalid(field, "not a challenge this issued"))
}

fn registration(r: Option<&pb::PasskeyRegistration>) -> Result<NewRegistration, ConnectError> {
    let r = r.ok_or_else(|| error::invalid("registration", "required"))?;
    Ok(NewRegistration {
        challenge: challenge_id("registration.challenge_id", &r.challenge_id)?,
        label: r.label.clone(),
        client_data_json: r.client_data_json.clone(),
        attestation_object: r.attestation_object.clone(),
    })
}

fn redeemed(r: Redeemed) -> pb::RedeemResponse {
    pb::RedeemResponse {
        account: wire_id(IdKind::User, r.account),
        recovery_codes: r.recovery_codes,
        ..Default::default()
    }
}

fn signed_in(account: Uuid) -> pb::SignInResponse {
    pb::SignInResponse { account: wire_id(IdKind::User, account), ..Default::default() }
}

fn ip(ctx: &RequestContext) -> Option<IpAddr> {
    ctx.peer_addr().map(|a| a.ip())
}

impl Api {
    /// `result`, or its refusal as a failed `attempt` from `ip`.
    async fn attempted<T>(
        &self,
        result: Result<T, AppError>,
        attempt: Attempt,
        ip: Option<IpAddr>,
    ) -> Result<T, ConnectError> {
        match result {
            Ok(t) => Ok(t),
            Err(e) => Err(self.failed(e, (attempt, ip, None)).await),
        }
    }
}

impl pb::AccountService for Api {
    async fn get_invitation(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetInvitationRequest>,
    ) -> ServiceResult<pb::GetInvitationResponse> {
        let seen = self.app.invitation(request.invitation).await;
        let view = self.attempted(seen, Attempt::Invitation, ip(&ctx)).await?;
        Response::ok(pb::GetInvitationResponse {
            display_name: view.display_name,
            expires_at: Some(timestamp(view.expires_at)).into(),
            passkey_only: view.passkey_only,
            ..Default::default()
        })
    }

    async fn begin_redemption(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::BeginRedemptionRequest>,
    ) -> ServiceResult<pb::BeginRedemptionResponse> {
        let begun = self.app.begin_redemption(request.invitation, request.name).await;
        let options = self.attempted(begun, Attempt::Invitation, ip(&ctx)).await?;
        Response::ok(pb::BeginRedemptionResponse {
            creation: creation(options).into(),
            ..Default::default()
        })
    }

    async fn redeem_with_passkey(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RedeemWithPasskeyRequest>,
    ) -> ServiceResult<pb::RedeemResponse> {
        let req = request.to_owned_message();
        let r = registration(req.registration.as_option())?;
        let done = self.app.redeem_with_passkey(&req.invitation, &req.name, r).await;
        Response::ok(redeemed(self.attempted(done, Attempt::Invitation, ip(&ctx)).await?))
    }

    #[expect(clippy::unused_async_trait_impl, reason = "the generated trait is async")]
    async fn offer_totp(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::OfferTotpRequest>,
    ) -> ServiceResult<pb::OfferTotpResponse> {
        let secret = TotpSecret::generate().map_err(|e| error::internal(&e))?;
        Response::ok(pb::OfferTotpResponse {
            secret: secret.bytes().to_vec(),
            uri: secret.uri(self.app.passkey_site().unwrap_or(ISSUER), request.name),
            ..Default::default()
        })
    }

    async fn redeem_with_password(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::RedeemWithPasswordRequest>,
    ) -> ServiceResult<pb::RedeemResponse> {
        let req = request.to_owned_message();
        let bytes: [u8; 20] = req
            .totp_secret
            .as_slice()
            .try_into()
            .map_err(|_| error::invalid("totp_secret", "the 20 bytes OfferTotp gave"))?;
        let r = PasswordRedemption {
            name: req.name.clone(),
            password: Zeroizing::new(req.password.clone()),
            totp: TotpSecret::from_bytes(bytes),
            code: req.totp_code.clone(),
        };
        let done = self.app.redeem_with_password(&req.invitation, r).await;
        Response::ok(redeemed(self.attempted(done, Attempt::Invitation, ip(&ctx)).await?))
    }

    async fn begin_sign_in(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, pb::BeginSignInRequest>,
    ) -> ServiceResult<pb::BeginSignInResponse> {
        let options = self.app.begin_sign_in().await.map_err(error::from_app)?;
        Response::ok(pb::BeginSignInResponse {
            request: pb::PasskeyRequest {
                challenge_id: options.challenge_id.to_string(),
                challenge: options.challenge.to_vec(),
                rp_id: options.rp_id,
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
    }

    async fn sign_in_with_passkey(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SignInWithPasskeyRequest>,
    ) -> ServiceResult<pb::SignInResponse> {
        let req = request.to_owned_message();
        let a = Assertion {
            challenge: challenge_id("challenge_id", &req.challenge_id)?,
            credential_id: req.credential_id,
            client_data_json: req.client_data_json,
            authenticator_data: req.authenticator_data,
            signature: req.signature,
        };
        let done = self.app.sign_in_with_passkey(a).await;
        Response::ok(signed_in(self.attempted(done, Attempt::SignIn, ip(&ctx)).await?.account))
    }

    async fn sign_in_with_password(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, pb::SignInWithPasswordRequest>,
    ) -> ServiceResult<pb::SignInResponse> {
        let req = request.to_owned_message();
        let second = match req.second {
            Some(Second::TotpCode(c)) => SecondFactor::Totp(c),
            Some(Second::RecoveryCode(c)) => SecondFactor::Recovery(c),
            None => return Err(error::invalid("second", "a TOTP code or a recovery code")),
        };
        let done = self.app.sign_in_with_password(&req.name, &req.password, second).await;
        Response::ok(signed_in(self.attempted(done, Attempt::SignIn, ip(&ctx)).await?.account))
    }
}
