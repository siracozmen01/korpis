// SPDX-License-Identifier: AGPL-3.0-or-later

//! An ID token's claims, validated as OIDC Core §3.1.3.7 says once its signature is: issued by
//! the configured issuer, for this client alone, not expired, issued within the skew of now,
//! and, for a sign-in, carrying the nonce the control plane made. `sub` is the person, the one
//! identifier OIDC says a client may rely on beside `iss` (§5.7).

use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How far `iat` may be from now: the skew assertion allows (§5.2 of docs/08-identity.md).
pub const SKEW: Duration = Duration::from_secs(5 * 60);
/// How far past `exp` a token is still taken, for clocks that differ by a little.
pub const LEEWAY: Duration = Duration::from_secs(60);
/// The longest `sub` OIDC allows (OIDC Core §2).
const MAX_SUB: usize = 255;

/// What a token must say.
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    /// The issuer, exactly.
    pub issuer: &'a str,
    /// This client.
    pub client_id: &'a str,
    /// The nonce of the sign-in; `None` for a token from a refresh, which carries none.
    pub nonce: Option<&'a str>,
    /// The claim naming the person's groups.
    pub groups: &'a str,
    /// Now, by this provider's clock.
    pub now: SystemTime,
}

/// The person a token names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// `sub`.
    pub sub: String,
    /// The values of the group claim; none when the token carries it not.
    pub groups: Vec<String>,
    /// `iat`.
    pub issued_at: SystemTime,
}

fn instant(claims: &Value, name: &str) -> Result<SystemTime, String> {
    let at = claims.get(name).and_then(Value::as_u64).ok_or_else(|| format!("no {name}"))?;
    Ok(UNIX_EPOCH + Duration::from_secs(at))
}

fn audience_is(claims: &Value, client: &str) -> bool {
    let only = |v: &Value| v.as_str() == Some(client);
    let aud = match claims.get("aud") {
        Some(Value::Array(a)) => !a.is_empty() && a.iter().all(only),
        Some(v) => only(v),
        None => false,
    };
    aud && claims.get("azp").is_none_or(only)
}

fn groups(claims: &Value, name: &str) -> Result<Vec<String>, String> {
    let bad = || format!("the {name} claim is not a list of names");
    match claims.get(name) {
        None => Ok(Vec::new()),
        Some(Value::String(g)) => Ok(vec![g.clone()]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|g| g.as_str().filter(|g| !g.is_empty()).map(str::to_owned).ok_or_else(bad))
            .collect(),
        Some(_) => Err(bad()),
    }
}

/// The person `claims` names, if they say what `expected` asks.
///
/// # Errors
/// A claim is missing or says otherwise, naming which.
pub fn validated(claims: &Value, expected: &Expected<'_>) -> Result<Person, String> {
    if claims.get("iss").and_then(Value::as_str) != Some(expected.issuer) {
        return Err("it was issued by another issuer".into());
    }
    if !audience_is(claims, expected.client_id) {
        return Err("it was issued for another client".into());
    }
    if expected.now > instant(claims, "exp")? + LEEWAY {
        return Err("it has expired".into());
    }
    let issued_at = instant(claims, "iat")?;
    let apart = expected
        .now
        .duration_since(issued_at)
        .or_else(|_| issued_at.duration_since(expected.now))
        .unwrap_or_default();
    if apart > SKEW {
        return Err("it was issued too far from now".into());
    }
    if let Some(nonce) = expected.nonce
        && claims.get("nonce").and_then(Value::as_str) != Some(nonce)
    {
        return Err("it carries another sign-in's nonce".into());
    }
    let sub = claims.get("sub").and_then(Value::as_str).unwrap_or_default();
    if sub.is_empty() || sub.len() > MAX_SUB {
        return Err("it names nobody".into());
    }
    Ok(Person { sub: sub.to_owned(), groups: groups(claims, expected.groups)?, issued_at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn expected(nonce: Option<&str>) -> Expected<'_> {
        let issuer = "https://idp.example";
        Expected { issuer, client_id: "korpis", nonce, groups: "groups", now: now() }
    }

    fn token() -> Value {
        json!({
            "iss": "https://idp.example", "aud": "korpis", "sub": "dana",
            "iat": 1_800_000_000_u64, "exp": 1_800_000_600_u64, "nonce": "n1",
            "groups": ["ops", "oncall"],
        })
    }

    fn with(field: &str, value: Value) -> Value {
        let mut t = token();
        if let Some(o) = t.as_object_mut() {
            o.insert(field.into(), value);
        }
        t
    }

    #[test]
    fn a_token_is_believed_only_as_section_3_1_3_7_says() {
        let person = validated(&token(), &expected(Some("n1"))).unwrap();
        assert_eq!((person.sub.as_str(), person.groups.len()), ("dana", 2));
        let refused = [
            with("iss", json!("https://idp.example/")),
            with("aud", json!(["korpis", "other"])),
            with("azp", json!("other")),
            with("exp", json!(1_799_999_000_u64)),
            with("iat", json!(1_799_999_000_u64)),
            with("nonce", json!("n2")),
            with("sub", json!("")),
            with("groups", json!([1])),
        ];
        for t in refused {
            assert!(validated(&t, &expected(Some("n1"))).is_err(), "{t}");
        }
        let refreshed = with("nonce", json!(null));
        assert!(validated(&refreshed, &expected(None)).is_ok(), "a refresh carries no nonce");
        assert!(validated(&with("aud", json!(["korpis"])), &expected(None)).is_ok());
    }
}
