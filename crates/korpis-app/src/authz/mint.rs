// SPDX-License-Identifier: AGPL-3.0-or-later

//! Minting a capability token: a grant's authority in a form verifiers check offline (§6 of
//! docs/08-identity.md). Minting from a use-limited grant spends one use, whatever the token
//! carries, because opening a session is the redemption; minting a session token's successor
//! while it lives, which is how a session is renewed, spends none (§6.1).

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::renewal::{Renewed, renewed};
use korpis_core::authz::route::RouteKey;
use korpis_core::authz::token::{self, Claims, KeyRing, SESSION_TOKEN_LIFETIME};
use korpis_core::authz::{Action, Decision, Kind, ObjectRef};
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
    /// The live session token this one renews: same holder, same chain, same object, no action
    /// it did not carry.
    pub renews: Option<String>,
}

/// A minted token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minted {
    /// The token.
    pub token: String,
    /// When it stops working.
    pub expires_at: SystemTime,
    /// For a token on a workload that is placed, the node a relay carries its sessions to,
    /// sealed so the holder cannot read it (§3.1 of docs/02-architecture.md).
    pub route: Option<String>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Mints a capability token for actions the caller may perform now.
    pub async fn mint_token(&self, caller: &Caller, cmd: MintToken) -> Result<Minted, AppError> {
        if cmd.actions.is_empty() {
            return Err(AppError::invalid("actions", "at least one action"));
        }
        let renewal =
            cmd.renews.as_deref().map(|t| self.renewal_of(caller, &cmd, t)).transpose()?;
        // Every action must be permitted now, through one chain: the token states one chain.
        let (mut chain, mut first) = (None, None);
        for name in &cmd.actions {
            let action =
                Action::parse(name).map_err(|e| AppError::invalid("actions", e.to_string()))?;
            let on = (&action, cmd.object);
            match self.decide_as(caller, on, cmd.via, renewal.is_some()).await? {
                Decision::Permit(p) if chain.as_ref().is_none_or(|(c, _)| *c == p.chain) => {
                    chain = Some((p.chain.clone(), p.source_cidr.clone()));
                    first.get_or_insert((action, p));
                }
                _ => return Err(self.refuse(caller, &action, cmd.object).await),
            }
        }
        if renewal.is_some_and(|r| chain.as_ref().is_none_or(|(c, _)| *c != r.chain)) {
            return Err(AppError::invalid("renews", "a renewal continues the same chain"));
        }
        // Every action came through one chain, so one leaf, and its range is the token's.
        let stream_range = first.as_ref().and_then(|(_, p)| p.stream_range);
        // One use for the token, whatever it carries; none for a renewal, whose permit says so.
        if let Some((action, permit)) = first {
            self.redeem(caller, &action, cmd.object, permit).await?;
        }
        let ttl = cmd.ttl.map_or(SESSION_TOKEN_LIFETIME, |t| t.min(SESSION_TOKEN_LIFETIME));
        let expires_at = self.clock.now() + ttl;
        let (chain, source_cidr) = chain.unwrap_or_default();
        let claims = Claims {
            subject: caller.subject.key.clone(),
            chain,
            actions: cmd.actions,
            scope: cmd.object,
            expires_at,
            // The relay checks the client against them; the gateway checked the caller already.
            source_cidr,
            // The node reading the stream holds the session to it.
            stream_range,
        };
        let token =
            token::mint(&self.token_key, &claims).map_err(|e| AppError::Internal(e.to_string()))?;
        let route = self.route(cmd.object).await?;
        // Minting acts on nothing: it records no effect. Using the token is recorded where it is
        // used, under the chain it names.
        Ok(Minted { token, expires_at, route })
    }

    /// What `token`, presented as the session token `cmd` renews, says, once it is shown to be
    /// one this caller may renew into `cmd`.
    fn renewal_of(
        &self,
        caller: &Caller,
        cmd: &MintToken,
        token: &str,
    ) -> Result<Renewed, AppError> {
        let mut keys = KeyRing::default();
        keys.insert(self.token_key.key_id, &self.token_key.public_bytes())
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let r = renewed(token, &keys, self.clock.now())
            .map_err(|_| AppError::invalid("renews", "not a live session token of this server"))?;
        if r.subject != caller.subject.key || r.scope != cmd.object {
            return Err(AppError::invalid(
                "renews",
                "a renewal is the same holder's, on its object",
            ));
        }
        if !cmd.actions.iter().all(|a| r.actions.contains(a)) {
            return Err(AppError::invalid(
                "renews",
                "a renewal carries no action the token did not",
            ));
        }
        Ok(r)
    }

    /// The sealed route to the node `object` is placed on, if it is a placed workload. Sealed
    /// whoever asks: a route is what any session needs and tells its holder nothing, so who may
    /// read the node never decides who may reach the workload.
    async fn route(&self, object: ObjectRef) -> Result<Option<String>, AppError> {
        if object.kind != Kind::Workload {
            return Ok(None);
        }
        let Some(placed) = self.store.placement(object.id).await? else { return Ok(None) };
        RouteKey::of(&self.token_key)
            .seal(object.id, placed.node)
            .map(Some)
            .map_err(|e| AppError::Internal(e.to_string()))
    }
}
