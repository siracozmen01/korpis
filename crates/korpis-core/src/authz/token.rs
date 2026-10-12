// SPDX-License-Identifier: AGPL-3.0-or-later

//! Capability tokens: the wire form of a grant (§6 of docs/08-identity.md).
//!
//! A token is a Biscuit whose authority block, signed by the control plane's token key, states
//! the grant chain it was minted from, the actions, the scope, and the expiry. Relays and agents
//! verify it offline: signature, expiry, scope, action, and the revocation list, which names
//! grant identifiers so that revoking any link of the chain refuses the token (§6.3). A holder
//! may attenuate it further by appending blocks, which can only add checks.
//!
//! The gateway mints a token only after [`super::evaluate`] has permitted the request it covers,
//! so the online-only conditions (`max_uses`, `requires_mfa`, `requires_approval_by`) are settled
//! before a token exists, and tokens never carry them (§6.2).

use super::scope::{Kind, ObjectRef, Resource};
use biscuit_auth::builder::{Algorithm, Term, date, string};
use biscuit_auth::{AuthorizerBuilder, AuthorizerLimits, Biscuit, KeyPair, PublicKey, error};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::hash::BuildHasher;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// The control plane's token signing key. The key id is the key epoch: rotating it (on restore,
/// or on suspected compromise) invalidates every token signed by the previous one.
#[derive(Debug)]
pub struct SigningKey {
    /// Which key it is, so verifiers can hold several during rotation.
    pub key_id: u32,
    keypair: KeyPair,
}

impl SigningKey {
    /// A fresh key.
    #[must_use]
    pub fn generate(key_id: u32) -> Self {
        Self { key_id, keypair: KeyPair::new_with_algorithm(Algorithm::Ed25519) }
    }

    /// Restores a key from its 32 private bytes.
    pub fn from_bytes(key_id: u32, private: &[u8]) -> Result<Self, TokenError> {
        let private = biscuit_auth::PrivateKey::from_bytes(private, Algorithm::Ed25519)
            .map_err(|e| TokenError::Key(e.to_string()))?;
        let keypair = KeyPair::from(&private);
        Ok(Self { key_id, keypair })
    }

    /// The 32 private bytes, for storage outside the database (§6 of docs/18-operations.md).
    #[must_use]
    pub fn private_bytes(&self) -> Vec<u8> {
        self.keypair.private().to_bytes().to_vec()
    }

    /// The public half, for verifiers.
    #[must_use]
    pub fn public_bytes(&self) -> Vec<u8> {
        self.keypair.public().to_bytes()
    }
}

/// The verify-only keys a relay or agent holds: current and historical, by key id.
#[derive(Debug, Default, Clone)]
pub struct KeyRing {
    keys: BTreeMap<u32, PublicKey>,
}

impl KeyRing {
    /// Trusts a public key under `key_id`.
    pub fn insert(&mut self, key_id: u32, public: &[u8]) -> Result<(), TokenError> {
        let key = PublicKey::from_bytes(public, Algorithm::Ed25519)
            .map_err(|e| TokenError::Key(e.to_string()))?;
        self.keys.insert(key_id, key);
        Ok(())
    }

    /// Forgets a key, so that every token it signed stops verifying.
    pub fn retire(&mut self, key_id: u32) {
        self.keys.remove(&key_id);
    }
}

/// What a token asserts. The scope is always one concrete object: selector scopes are resolved
/// by the gateway before minting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    /// The holder's subject key.
    pub subject: String,
    /// The grant chain, leaf first, root last.
    pub chain: Vec<Uuid>,
    /// The actions it carries.
    pub actions: Vec<String>,
    /// The one object it is for.
    pub scope: ObjectRef,
    /// When it stops working.
    pub expires_at: SystemTime,
}

/// Why a token was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// A key is malformed or unknown.
    #[error("key: {0}")]
    Key(String),
    /// The token is malformed or its signature does not verify.
    #[error("malformed or unverifiable token: {0}")]
    Invalid(String),
    /// The token does not permit the request.
    #[error("the token does not authorize this request: {0}")]
    Unauthorized(String),
    /// A grant on its chain is revoked.
    #[error("grant {0} on the token's chain is revoked")]
    Revoked(Uuid),
    /// The token names no chain.
    #[error("the token states no chain")]
    NoChain,
}

impl From<error::Token> for TokenError {
    fn from(e: error::Token) -> Self {
        Self::Invalid(e.to_string())
    }
}

/// Mints a token for `claims`, signed by `key`, encoded for transport (URL-safe base64).
pub fn mint(key: &SigningKey, claims: &Claims) -> Result<String, TokenError> {
    let mut code = String::new();
    let mut params: HashMap<String, Term> = HashMap::new();
    params.insert("subject".into(), string(&claims.subject));
    params.insert("kind".into(), string(claims.scope.kind.as_str()));
    params.insert("id".into(), string(&claims.scope.id.to_string()));
    params.insert("exp".into(), date(&claims.expires_at));
    code.push_str("subject({subject});\nscope({kind}, {id});\nexpires({exp});\n");
    code.push_str("check if time($t), $t < {exp};\n");
    for (i, grant) in claims.chain.iter().enumerate() {
        let name = format!("g{i}");
        let _ = writeln!(code, "grant({{{name}}});");
        params.insert(name, string(&grant.to_string()));
    }
    for (i, action) in claims.actions.iter().enumerate() {
        let name = format!("a{i}");
        let _ = writeln!(code, "right({{{name}}});");
        params.insert(name, string(action));
    }
    if let Some(leaf) = claims.chain.first() {
        code.push_str("leaf({leaf});\n");
        params.insert("leaf".into(), string(&leaf.to_string()));
    }
    let token = Biscuit::builder()
        .code_with_params(&code, params, HashMap::new())?
        .root_key_id(key.key_id)
        .build(&key.keypair)?;
    Ok(token.to_base64()?)
}

/// Bounds on one verification. Biscuit's defaults cap evaluation at one millisecond, which a
/// loaded relay exceeds, refusing valid tokens; the time bound here is generous because the
/// fact and iteration bounds are what keep a hostile token cheap, and they stay at Biscuit's
/// defaults.
const VERIFY_LIMITS: AuthorizerLimits =
    AuthorizerLimits { max_facts: 1000, max_iterations: 100, max_time: Duration::from_millis(100) };

/// What a verified token permits, as far as the verifier needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The holder's subject key.
    pub subject: String,
    /// The grant the request acts under: the leaf of the chain, which every effect names.
    pub grant: Uuid,
    /// The grant chain, leaf first.
    pub chain: Vec<Uuid>,
}

/// Verifies `token` offline for `action` on `resource` at `now`, against `keys` and the revoked
/// grant identifiers. This is what a relay and an agent run; it needs no database.
pub fn verify<S: BuildHasher>(
    token: &str,
    keys: &KeyRing,
    action: &str,
    resource: &Resource,
    revoked: &HashSet<Uuid, S>,
    now: SystemTime,
) -> Result<Verified, TokenError> {
    let biscuit = Biscuit::from_base64(token, |id: Option<u32>| {
        id.and_then(|id| keys.keys.get(&id).copied()).ok_or(error::Format::UnknownPublicKey)
    })?;

    let mut code = String::from("operation({op});\ntime({now});\n");
    let mut params: HashMap<String, Term> = HashMap::new();
    params.insert("op".into(), string(action));
    params.insert("now".into(), date(&now));
    for (i, node) in resource.chain.iter().enumerate() {
        let _ = writeln!(code, "resource({{k{i}}}, {{i{i}}});");
        params.insert(format!("k{i}"), string(node.object.kind.as_str()));
        params.insert(format!("i{i}"), string(&node.object.id.to_string()));
    }
    code.push_str("allow if operation($a), right($a), resource($k, $i), scope($k, $i);\n");
    let mut authorizer = AuthorizerBuilder::new()
        .code_with_params(&code, params, HashMap::new())?
        .set_limits(VERIFY_LIMITS)
        .build(&biscuit)?;
    authorizer.authorize().map_err(|e| TokenError::Unauthorized(e.to_string()))?;

    // A private action does not reach into an organization from a scope above it, offline
    // exactly as online (§3.3 of docs/08-identity.md): the token's scope must sit at or below
    // the resource's owning organization in the resource's own chain.
    if super::action::PRIVATE.contains(&action) {
        let scope: Vec<(String, String)> = authorizer.query("data($k, $i) <- scope($k, $i)")?;
        let owner_at = resource.chain.iter().position(|n| n.object.kind == Kind::Organization);
        let scope_at = scope.first().and_then(|(k, i)| {
            resource
                .chain
                .iter()
                .position(|n| n.object.kind.as_str() == k && n.object.id.to_string() == *i)
        });
        if !matches!((scope_at, owner_at), (Some(s), Some(o)) if s <= o) {
            return Err(TokenError::Unauthorized(
                "a private action does not reach into an organization from above".into(),
            ));
        }
    }
    let chain: Vec<(String,)> = authorizer.query("chain($g) <- grant($g)")?;
    let chain: Vec<Uuid> = chain.into_iter().filter_map(|(g,)| Uuid::parse_str(&g).ok()).collect();
    if let Some(r) = chain.iter().find(|g| revoked.contains(g)) {
        return Err(TokenError::Revoked(*r));
    }
    let leaf: Vec<(String,)> = authorizer.query("data($g) <- leaf($g)")?;
    let leaf = leaf.first().and_then(|(g,)| Uuid::parse_str(g).ok()).ok_or(TokenError::NoChain)?;
    let subject: Vec<(String,)> = authorizer.query("data($s) <- subject($s)")?;
    let subject = subject.into_iter().next().map(|(s,)| s).unwrap_or_default();
    Ok(Verified { subject, grant: leaf, chain })
}

/// Appends a block restricting the token to expire sooner: offline attenuation by its holder.
pub fn restrict_expiry(
    token: &str,
    keys: &KeyRing,
    expires_at: SystemTime,
) -> Result<String, TokenError> {
    let biscuit = Biscuit::from_base64(token, |id: Option<u32>| {
        id.and_then(|id| keys.keys.get(&id).copied()).ok_or(error::Format::UnknownPublicKey)
    })?;
    let mut params: HashMap<String, Term> = HashMap::new();
    params.insert("exp".into(), date(&expires_at));
    let block = biscuit_auth::BlockBuilder::new().code_with_params(
        "check if time($t), $t < {exp};",
        params,
        HashMap::new(),
    )?;
    Ok(biscuit.append(block)?.to_base64()?)
}

/// The default and maximum lifetime of a session token (§6 of docs/08-identity.md): revocation
/// latency equals the token lifetime, so it is short.
pub const SESSION_TOKEN_LIFETIME: Duration = Duration::from_secs(120);

/// A convenience for verifiers that know only an object's kind and id and its ancestry.
#[must_use]
pub fn object_kind(kind: &str) -> Option<Kind> {
    Kind::parse(kind)
}
