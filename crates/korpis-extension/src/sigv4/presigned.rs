// SPDX-License-Identifier: Apache-2.0

//! A request presigned in its query: `X-Amz-Algorithm`, `X-Amz-Credential`, `X-Amz-Date`,
//! `X-Amz-Expires`, `X-Amz-SignedHeaders`, an optional `X-Amz-Security-Token`, and
//! `X-Amz-Signature`, its payload unsigned. Its canonical request is rebuilt from what arrived:
//! the path as sent, the other query parameters decoded, sorted, and encoded again, and each
//! header it names with its value.

use super::{Signer, header_lines};
use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How far in the future a request may say it was signed: clocks differ.
const SKEW: Duration = Duration::from_secs(300);
/// The longest a presigned request may be valid, which is S3's own limit.
const LONGEST: Duration = Duration::from_secs(7 * 24 * 3600);

/// A request as it arrived.
#[derive(Debug, Clone, Copy)]
pub struct Presigned<'a> {
    /// Its method.
    pub method: &'a str,
    /// Its path, still encoded.
    pub path: &'a str,
    /// Its query, still encoded.
    pub query: &'a str,
    /// Its headers, lower-case names.
    pub headers: &'a [(String, String)],
}

/// What a request was signed as, once its signature holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The access key it names.
    pub access_key_id: String,
    /// The session token it carries; empty if none.
    pub session_token: String,
    /// The region it was signed for.
    pub region: String,
    /// The query's other parameters, decoded, in the order they arrived.
    pub parameters: Vec<(String, String)>,
}

/// Why a request is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// It is not a presigned request, or one this verifies.
    #[error("not a presigned request: {0}")]
    Malformed(&'static str),
    /// It is past its lifetime, or claims a time to come.
    #[error("the request is not valid now")]
    Expired,
    /// Its access key is unknown.
    #[error("the access key is unknown")]
    UnknownKey,
    /// Its signature is not the one its key makes.
    #[error("the signature does not match")]
    Signature,
}

/// Decodes `%XX` escapes; anything else is itself.
fn decode(s: &str) -> Result<String, Refusal> {
    let mut out = Vec::with_capacity(s.len());
    let mut rest = s.as_bytes();
    while let Some((&first, after)) = rest.split_first() {
        rest = after;
        if first != b'%' {
            out.push(first);
            continue;
        }
        let (digits, after) = rest.split_at_checked(2).ok_or(Refusal::Malformed("an escape"))?;
        let digits = std::str::from_utf8(digits).map_err(|_| Refusal::Malformed("an escape"))?;
        out.push(u8::from_str_radix(digits, 16).map_err(|_| Refusal::Malformed("an escape"))?);
        rest = after;
    }
    String::from_utf8(out).map_err(|_| Refusal::Malformed("a parameter is not text"))
}

/// Encodes `s` as a signature's canonical query does: every byte but the unreserved.
fn encode(s: &str) -> String {
    s.bytes().fold(String::with_capacity(s.len()), |mut out, b| {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
        out
    })
}

/// The instant `YYYYMMDDTHHMMSSZ` names.
fn instant(amz_date: &str) -> Option<SystemTime> {
    let shaped = amz_date.len() == 16
        && amz_date.get(8..9) == Some("T")
        && amz_date.get(15..16) == Some("Z");
    if !shaped {
        return None;
    }
    let field = |at: std::ops::Range<usize>| amz_date.get(at)?.parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(4..6)?, field(6..8)?);
    let (hour, minute, second) = (field(9..11)?, field(11..13)?, field(13..15)?);
    let valid = (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour < 24
        && minute < 60
        && second <= 60;
    if !valid {
        return None;
    }
    // Days from the civil date, by Howard Hinnant's algorithm.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468;
    let secs = u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

/// Whether two signatures are equal, in time that does not depend on where they differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The parameters of `query`, decoded, and the signature apart from them.
fn split(query: &str) -> Result<(Vec<(String, String)>, String), Refusal> {
    let mut pairs = Vec::new();
    let mut signature = None;
    for part in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        let (k, v) = (decode(k)?, decode(v)?);
        if k == "X-Amz-Signature" {
            signature = Some(v);
        } else {
            pairs.push((k, v));
        }
    }
    Ok((pairs, signature.ok_or(Refusal::Malformed("no signature"))?))
}

/// Verifies `r` at `now`, the secret of its access key from `secret_of`.
///
/// # Errors
/// What [`Refusal`] names.
pub fn verify_presigned(
    r: &Presigned<'_>,
    now: SystemTime,
    secret_of: impl FnOnce(&str) -> Option<String>,
) -> Result<Verified, Refusal> {
    let (parameters, signature) = split(r.query)?;
    let get = |name: &str| parameters.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    if get("X-Amz-Algorithm") != Some("AWS4-HMAC-SHA256") {
        return Err(Refusal::Malformed("the algorithm"));
    }
    let credential = get("X-Amz-Credential").ok_or(Refusal::Malformed("no credential"))?;
    let scope: Vec<&str> = credential.split('/').collect();
    let [key, day, region, "s3", "aws4_request"] = scope[..] else {
        return Err(Refusal::Malformed("the credential's scope"));
    };
    let amz_date = get("X-Amz-Date").ok_or(Refusal::Malformed("no date"))?;
    let signed_at = instant(amz_date).ok_or(Refusal::Malformed("the date"))?;
    if !amz_date.starts_with(day) {
        return Err(Refusal::Malformed("the date is not the scope's"));
    }
    let expires = get("X-Amz-Expires").and_then(|e| e.parse().ok()).map(Duration::from_secs);
    let expires = expires.filter(|e| *e <= LONGEST).ok_or(Refusal::Malformed("the lifetime"))?;
    if signed_at > now + SKEW || signed_at + expires < now {
        return Err(Refusal::Expired);
    }
    let names = get("X-Amz-SignedHeaders").ok_or(Refusal::Malformed("no signed headers"))?;
    let mut headers = Vec::new();
    for name in names.split(';') {
        let value = r.headers.iter().find(|(n, _)| n == name);
        let value = value.ok_or(Refusal::Malformed("a signed header is missing"))?;
        headers.push((name, value.1.as_str()));
    }
    let mut sorted: Vec<(String, String)> =
        parameters.iter().map(|(k, v)| (encode(k), encode(v))).collect();
    sorted.sort();
    let query: Vec<String> = sorted.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let (lines, signed) = header_lines(&headers);
    let canonical = format!(
        "{}\n{}\n{}\n{lines}\n{signed}\nUNSIGNED-PAYLOAD",
        r.method,
        r.path,
        query.join("&")
    );
    let secret = secret_of(key).ok_or(Refusal::UnknownKey)?;
    let by = Signer { access_key_id: key, secret: &secret, region, service: "s3" };
    if !same(&by.signature(&canonical, amz_date), &signature) {
        return Err(Refusal::Signature);
    }
    let session_token = get("X-Amz-Security-Token").unwrap_or_default().to_owned();
    let (access_key_id, region) = (key.to_owned(), region.to_owned());
    Ok(Verified { access_key_id, session_token, region, parameters })
}

#[cfg(test)]
mod tests;
