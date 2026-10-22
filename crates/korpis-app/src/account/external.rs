// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in through an identity provider (§9.7 of docs/08-identity.md), begun: which providers
//! the installation's sign-in page offers, and a sign-in's state, nonce, and address. Neither
//! needs a credential; the server charges each to its source (§10 of docs/10-api.md).

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{Clock, ProviderCallWrite as _, RecipeSource, SignInWrite as _, Store, Tx as _};
use korpis_core::authz::binding::ProviderRef;
use korpis_core::authz::sign_in::{
    Opened, PENDING_SIGN_IN, PendingSignIn, is_challenge, redirect_uri,
};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::extension::identity::SignIn;
use korpis_core::extension::{CallOutcome, CallRequest, ProviderType};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long an identity provider has to say where to send a person: they are waiting.
const AUTHORIZE_CALL: Duration = Duration::from_secs(10);

/// Where to send a person to sign in, and until when they may return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInBegun {
    /// The address at the identity provider.
    pub url: String,
    /// When the sign-in stops being completed.
    pub expires_at: SystemTime,
}

/// `provider` as a call's target.
pub(super) fn target(provider: &ProviderRef) -> CallTarget {
    CallTarget {
        organization: provider.organization,
        kind: ProviderType::Identity,
        name: provider.name.clone(),
    }
}

/// The provider's refusal, or the control plane's failure to reach it: either fails the sign-in
/// closed (§5 of docs/16-extensions.md), and says which.
pub(super) fn unavailable(outcome: CallOutcome) -> AppError {
    let detail = match outcome {
        CallOutcome::Failed(f) => f.reason,
        _ => "provider.malformed".into(),
    };
    AppError::Unavailable { what: "the identity provider", detail }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The identity providers that sign people in, by name, of the organization whose sign-in
    /// page it is: at `portal` the portal's organization (§9.8 of docs/08-identity.md), and
    /// elsewhere the installation's root organization. A provider anywhere else is reached by
    /// its own link.
    pub async fn sign_in_providers(
        &self,
        portal: Option<Uuid>,
    ) -> Result<Vec<ProviderRef>, AppError> {
        let offering = match portal {
            Some(p) => self.portal_organization(p).await?,
            None => self.store.installation_root().await?,
        };
        let Some(root) = offering else { return Ok(Vec::new()) };
        let mut names: Vec<String> = self
            .store
            .registrations_in(root)
            .await?
            .into_iter()
            .filter(|r| r.kind == ProviderType::Identity && r.sign_in)
            .map(|r| r.name)
            .collect();
        names.sort();
        names.dedup();
        Ok(names.into_iter().map(|name| ProviderRef { organization: root, name }).collect())
    }

    /// Begins a sign-in through `provider` for a panel at `portal`, or at the installation's own
    /// address, that keeps the PKCE verifier of `code_challenge`: where to send the person, who
    /// comes back to the address it began at. Refused without a panel to return to, for a
    /// provider that does not sign people in, and at a portal for one not at or beneath its
    /// organization.
    pub async fn begin_external_sign_in(
        &self,
        provider: &ProviderRef,
        code_challenge: &str,
        portal: Option<Uuid>,
    ) -> Result<SignInBegun, AppError> {
        if !is_challenge(code_challenge) {
            return Err(AppError::invalid("code_challenge", "an S256 PKCE challenge"));
        }
        let redirect_uri = redirect_uri(&self.returned_to(provider, portal).await?);
        self.signs_in(provider).await?;
        let opened = Opened::new().map_err(|e| AppError::Internal(e.to_string()))?;
        let request = CallRequest::Authorize(SignIn {
            state: opened.state,
            nonce: opened.nonce.clone(),
            code_challenge: code_challenge.to_owned(),
            redirect_uri: redirect_uri.clone(),
        });
        let called = self.call_own_provider(&target(provider), request, AUTHORIZE_CALL).await?;
        let now = self.clock.now();
        let pending = PendingSignIn {
            provider: provider.clone(),
            nonce: opened.nonce,
            code_challenge: code_challenge.to_owned(),
            redirect_uri,
            portal,
            begun_at: now,
            expires_at: now + PENDING_SIGN_IN,
        };
        let mut tx = self.store.begin().await?;
        tx.redact_sign_in_call(called.id).await?;
        let CallOutcome::Authorization(a) = called.outcome else {
            tx.commit().await?;
            return Err(unavailable(called.outcome));
        };
        tx.insert_pending_sign_in(&opened.state_digest, &pending).await?;
        tx.commit().await?;
        Ok(SignInBegun { url: a.url, expires_at: pending.expires_at })
    }

    /// The origin a sign-in through `provider` begun at `portal` comes back to: the portal's,
    /// for a provider at or beneath the portal's organization, a provider elsewhere being
    /// refused there as if it did not exist; and the panel's own at the installation's address.
    async fn returned_to(
        &self,
        provider: &ProviderRef,
        portal: Option<Uuid>,
    ) -> Result<String, AppError> {
        let Some(portal) = portal else {
            let origin = self.panel_origin().ok_or_else(|| AppError::Unavailable {
                what: "signing in through an identity provider",
                detail: "no --public-url is configured, so there is no panel to return to".into(),
            })?;
            return Ok(origin.to_owned());
        };
        let portal = self.store.portal(portal).await?.ok_or(AppError::Unauthenticated)?;
        let at = ObjectRef::new(Kind::Organization, portal.organization);
        let ancestry =
            self.store.ancestry(ObjectRef::new(Kind::Organization, provider.organization));
        if !ancestry.await?.iter().any(|n| n.object == at) {
            return Err(AppError::NotFound { kind: "identity provider that signs people in" });
        }
        Ok(portal.origin(self.public_port()))
    }

    /// Whether `provider` is registered, at its own organization, as signing people in.
    async fn signs_in(&self, provider: &ProviderRef) -> Result<(), AppError> {
        let registered = self
            .store
            .registrations(provider.organization, ProviderType::Identity, &provider.name)
            .await?;
        if registered.iter().any(|r| r.sign_in) {
            return Ok(());
        }
        Err(AppError::NotFound { kind: "identity provider that signs people in" })
    }
}
