// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in through an identity provider (§9.7 of docs/08-identity.md), the session service's
//! procedures that take no credential: which providers the sign-in page offers, a sign-in begun,
//! and one completed, which re-authenticates the caller's session if it has one.

use super::account::{client, with_cookie};
use super::wire::{self, Refuse as _};
use super::{Api, error};
use connectrpc::{ConnectError, RequestContext, Response, ServiceResult};
use korpis_app::account::{Completed, Completion};
use korpis_core::audit::failure::Attempt;
use korpis_core::authz::binding::ProviderRef;
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::{timestamp, wire_id};

impl Api {
    /// The providers the installation's sign-in page offers.
    pub(super) async fn sign_in_providers(
        &self,
    ) -> Result<pb::ListSignInProvidersResponse, ConnectError> {
        let providers = self.app.sign_in_providers().await.map_err(error::from_app)?;
        Ok(pb::ListSignInProvidersResponse {
            providers: providers
                .into_iter()
                .map(|p| pb::SignInProvider {
                    organization: wire_id(IdKind::Organization, p.organization),
                    name: p.name,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
    }

    /// Begins the sign-in `req` asks for.
    pub(super) async fn begin_external_sign_in(
        &self,
        req: &pb::BeginExternalSignInRequest,
    ) -> Result<pb::BeginExternalSignInResponse, ConnectError> {
        let organization =
            wire::id(IdKind::Organization, "organization", &req.organization).refuse()?;
        let provider = ProviderRef { organization, name: req.provider.clone() };
        let begun = self
            .app
            .begin_external_sign_in(&provider, &req.code_challenge)
            .await
            .map_err(error::from_app)?;
        Ok(pb::BeginExternalSignInResponse {
            url: begun.url,
            expires_at: Some(timestamp(begun.expires_at)).into(),
            ..Default::default()
        })
    }

    /// Completes the sign-in `req` returns to, from `ctx`'s caller if it presents a credential.
    pub(super) async fn complete_external_sign_in(
        &self,
        ctx: &RequestContext,
        req: pb::CompleteExternalSignInRequest,
    ) -> ServiceResult<pb::CompleteExternalSignInResponse> {
        let caller =
            if super::bearer(ctx).is_empty() { None } else { Some(self.caller(ctx).await?) };
        let c = Completion { state: req.state, code: req.code, code_verifier: req.code_verifier };
        let client = client(ctx);
        let ip = client.ip;
        let done = self.app.complete_external_sign_in(caller.as_ref(), c, &client).await;
        match self.attempted(done, Attempt::External, ip).await? {
            Completed::Begun { binding, session } => {
                let body = pb::CompleteExternalSignInResponse {
                    binding: wire_id(IdKind::Binding, binding),
                    session: wire_id(IdKind::Session, session.id),
                    ..Default::default()
                };
                with_cookie(body, &session)
            }
            Completed::Reauthenticated { binding, session, at } => {
                Response::ok(pb::CompleteExternalSignInResponse {
                    binding: wire_id(IdKind::Binding, binding),
                    session: wire_id(IdKind::Session, session),
                    reauthenticated_at: Some(timestamp(at)).into(),
                    ..Default::default()
                })
            }
        }
    }
}
