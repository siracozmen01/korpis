// SPDX-License-Identifier: AGPL-3.0-or-later

//! The browser's cookies (§9.4 and §9.6 of docs/08-identity.md): the session's refresh
//! credential, and a device cookie for each account the browser has signed in to. Each is one no
//! script can read (`HttpOnly`), sent only over TLS and never on a request another site starts
//! (`Secure`, `SameSite=Strict`), and `__Host-` prefixed, which makes the browser refuse one set
//! with a `Domain` or for any path but `/`: a workload served under a sibling name of the
//! installation's cannot plant one the control plane would read (Finding 72 of
//! docs/23-walkthroughs.md).

use http::{HeaderMap, HeaderValue};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// The refresh credential's cookie.
pub const REFRESH: &str = "__Host-korpis_refresh";
/// What every device cookie's name starts with; the account it names follows.
pub const DEVICE: &str = "__Host-korpis_device_";

/// The `Set-Cookie` value that gives the browser the refresh credential `secret`, until
/// `expires_at`.
pub fn set(secret: &str, expires_at: SystemTime, now: SystemTime) -> Option<HeaderValue> {
    set_named(REFRESH, secret, expires_at, now)
}

/// The `Set-Cookie` value that gives the browser the device cookie `value` for `account`, until
/// `expires_at`.
pub fn set_device(
    account: Uuid,
    value: &str,
    expires_at: SystemTime,
    now: SystemTime,
) -> Option<HeaderValue> {
    set_named(&format!("{DEVICE}{}", account.simple()), value, expires_at, now)
}

fn set_named(
    name: &str,
    value: &str,
    expires_at: SystemTime,
    now: SystemTime,
) -> Option<HeaderValue> {
    let max_age = expires_at.duration_since(now).unwrap_or(Duration::ZERO).as_secs();
    let value =
        format!("{name}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Strict");
    HeaderValue::from_str(&value).ok()
}

/// The `Set-Cookie` value that makes the browser forget the refresh credential.
#[must_use]
pub fn clear() -> HeaderValue {
    HeaderValue::from_static(
        "__Host-korpis_refresh=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Strict",
    )
}

/// The refresh credential the request's cookies carry, if one does.
pub fn read(headers: &HeaderMap) -> Option<String> {
    pairs(headers).find(|(name, _)| *name == REFRESH).map(|(_, value)| value.to_owned())
}

/// The device cookies the request's cookies carry.
pub fn devices(headers: &HeaderMap) -> Vec<String> {
    pairs(headers)
        .filter(|(name, _)| name.starts_with(DEVICE))
        .map(|(_, value)| value.to_owned())
        .collect()
}

fn pairs(headers: &HeaderMap) -> impl Iterator<Item = (&str, &str)> {
    headers
        .get_all(http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cookies_are_host_bound_and_read_back_from_among_others() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let account = Uuid::now_v7();
        let refresh = set("korpisrt_abc", now + Duration::from_secs(60), now).unwrap();
        let device = set_device(account, "korpisdc_x", now + Duration::from_secs(60), now).unwrap();
        for set in [refresh.to_str().unwrap(), device.to_str().unwrap()] {
            for part in ["__Host-", "HttpOnly", "Secure", "SameSite=Strict", "Path=/;"] {
                assert!(set.contains(part), "{set}");
            }
            assert!(set.contains("Max-Age=60") && !set.contains("Domain"), "{set}");
        }
        let mut headers = HeaderMap::new();
        let cookie = format!(
            "theme=dark; __Host-korpis_refresh=korpisrt_abc; {DEVICE}{}=korpisdc_x",
            account.simple()
        );
        headers.insert(http::header::COOKIE, HeaderValue::from_str(&cookie).unwrap());
        assert_eq!(read(&headers).as_deref(), Some("korpisrt_abc"));
        assert_eq!(devices(&headers), vec!["korpisdc_x".to_owned()]);
        assert_eq!(read(&HeaderMap::new()), None);
        assert!(clear().to_str().unwrap().contains("Max-Age=0"));
    }
}
