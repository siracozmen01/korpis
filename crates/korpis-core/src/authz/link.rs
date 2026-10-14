// SPDX-License-Identifier: AGPL-3.0-or-later

//! Share links (§6.1 of docs/08-identity.md): the bearer form of a link-form grant, carried in a
//! URL's fragment. A link is a Biscuit signed by the token key, so it dies with the key's epoch,
//! as every token does when a restored control plane rotates it (§6), and a link the restored
//! database no longer accounts for stops working with the rest.
//!
//! It is accepted by the gateway alone, online, where the grant it names is evaluated as it
//! stands: expiry, revocation, and the use it spends. A relay and an agent verify tokens offline
//! against a revocation list whose horizon is a session token's lifetime (§6.3), so a link they
//! accepted would outlive its own revocation by its whole lifetime. They cannot accept one: a
//! link states no right and no scope, so the session check of [`super::token::verify`], which
//! allows only a right the token states on a scope it states, refuses it for every action. Every
//! use of a link begins at the gateway anyway: an action is an API call, and a session needs
//! the route only the gateway seals.

use super::token::{KeyRing, SigningKey, TokenError};
use biscuit_auth::builder::{Term, date, string};
use biscuit_auth::{AuthorizerBuilder, AuthorizerLimits, Biscuit};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// What a link credential starts with, so the gateway knows it from an API token's secret and
/// secret scanners know it from noise.
pub const LINK_PREFIX: &str = "korpislk_";

/// Bounds on verifying one: a link states two facts.
const LIMITS: AuthorizerLimits =
    AuthorizerLimits { max_facts: 100, max_iterations: 20, max_time: Duration::from_millis(100) };

/// A verified link: the grant it is the bearer form of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// The link-form grant.
    pub grant: Uuid,
    /// When it stops working, the earliest any of its blocks states.
    pub expires_at: SystemTime,
}

/// Mints the link of `grant`, which expires at `expires_at`, signed by `key`.
pub fn mint(key: &SigningKey, grant: Uuid, expires_at: SystemTime) -> Result<String, TokenError> {
    let mut params: HashMap<String, Term> = HashMap::new();
    params.insert("grant".into(), string(&grant.to_string()));
    params.insert("exp".into(), date(&expires_at));
    let code = "link({grant});\nexpires({exp});\ncheck if time($t), $t < {exp};\n";
    let token = Biscuit::builder()
        .code_with_params(code, params, HashMap::new())?
        .root_key_id(key.key_id)
        .build(key.keypair())?;
    Ok(format!("{LINK_PREFIX}{}", token.to_base64()?))
}

/// Verifies `presented`, a link credential, at `now` against `keys`. Only the authority block,
/// which the control plane signed, can state the grant: a fact a holder appends in a later block
/// is not trusted by this check, so attenuating a link can shorten it and never repoint it.
pub fn verify(presented: &str, keys: &KeyRing, now: SystemTime) -> Result<Link, TokenError> {
    let token = presented
        .strip_prefix(LINK_PREFIX)
        .ok_or_else(|| TokenError::Invalid("not a link".into()))?;
    let biscuit = keys.parse(token)?;
    let mut params: HashMap<String, Term> = HashMap::new();
    params.insert("now".into(), date(&now));
    let mut authorizer = AuthorizerBuilder::new()
        .code_with_params("time({now});\nallow if link($g);", params, HashMap::new())?
        .set_limits(LIMITS)
        .build(&biscuit)?;
    authorizer.authorize().map_err(|e| TokenError::Unauthorized(e.to_string()))?;
    let grants: Vec<(String,)> = authorizer.query("data($g) <- link($g)")?;
    let [(grant,)] = grants.as_slice() else {
        return Err(TokenError::Invalid("a link names one grant".into()));
    };
    let grant = Uuid::parse_str(grant).map_err(|e| TokenError::Invalid(e.to_string()))?;
    let expiries: Vec<(SystemTime,)> = authorizer.query_all("data($t) <- expires($t)")?;
    let expires_at = expiries
        .into_iter()
        .map(|(t,)| t)
        .min()
        .ok_or_else(|| TokenError::Invalid("the link states no expiry".into()))?;
    Ok(Link { grant, expires_at })
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;
