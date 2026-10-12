// SPDX-License-Identifier: AGPL-3.0-or-later

//! Minting a capability token: a grant's authority in a form verifiers check offline (§6 of
//! docs/08-identity.md).

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, Store};
use korpis_core::authz::token::{self, Claims, SESSION_TOKEN_LIFETIME};
use korpis_core::authz::{Action, Decision, ObjectRef};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// A token to mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintToken {
    /// Mint from this grant only; any grant the caller holds when `None`.
    pub via: Option<Uuid>,
    /// What it carries; at least one, all permitted now through one chain.
    pub actions: Vec<String>,
    /// The one object it is for.
    pub object: ObjectRef,
    /// Requested lifetime; bounded by the server's maximum.
    pub ttl: Option<Duration>,
}

/// A minted token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minted {
    /// The token.
    pub token: String,
    /// When it stops working.
    pub expires_at: SystemTime,
}

impl<S: Store, C: Clock> App<S, C> {
    /// Mints a capability token for actions the caller may perform now.
    pub async fn mint_token(&self, caller: &Caller, cmd: MintToken) -> Result<Minted, AppError> {
        if cmd.actions.is_empty() {
            return Err(AppError::invalid("actions", "at least one action"));
        }
        // Every action must be permitted now, through one chain: the token states one chain.
        let mut chain = None;
        for name in &cmd.actions {
            let action =
                Action::parse(name).map_err(|e| AppError::invalid("actions", e.to_string()))?;
            match self.decide(caller, &action, cmd.object, cmd.via).await? {
                Decision::Permit(p)
                    if !p.redeem && chain.as_ref().is_none_or(|c| *c == p.chain) =>
                {
                    chain = Some(p.chain);
                }
                _ => return Err(self.refuse(caller, &action, cmd.object).await),
            }
        }
        let ttl = cmd.ttl.map_or(SESSION_TOKEN_LIFETIME, |t| t.min(SESSION_TOKEN_LIFETIME));
        let expires_at = self.clock.now() + ttl;
        let claims = Claims {
            subject: caller.subject.key.clone(),
            chain: chain.unwrap_or_default(),
            actions: cmd.actions,
            scope: cmd.object,
            expires_at,
        };
        let token =
            token::mint(&self.token_key, &claims).map_err(|e| AppError::Internal(e.to_string()))?;
        // Minting acts on nothing: it records no effect. Using the token is recorded where it is
        // used, under the chain it names.
        Ok(Minted { token, expires_at })
    }
}
