// SPDX-License-Identifier: AGPL-3.0-or-later

//! The browser's refresh credential (§9.4 of docs/08-identity.md): a cookie no script can read
//! (`HttpOnly`), sent only over TLS (`Secure`, which the `__Secure-` prefix makes the browser
//! insist on), never on a request another site starts (`SameSite=Strict`), and only to the
//! session service's path, so no other call carries it.

use http::{HeaderMap, HeaderValue};
use std::time::{Duration, SystemTime};

/// The cookie's name.
pub const REFRESH: &str = "__Secure-korpis_refresh";
/// The only path it is sent to: the session service's.
pub const PATH: &str = "/korpis.v1.SessionService/";

/// The `Set-Cookie` value that gives the browser `secret`, until `expires_at`.
pub fn set(secret: &str, expires_at: SystemTime, now: SystemTime) -> Option<HeaderValue> {
    let max_age = expires_at.duration_since(now).unwrap_or(Duration::ZERO).as_secs();
    let value = format!(
        "{REFRESH}={secret}; Path={PATH}; Max-Age={max_age}; HttpOnly; Secure; SameSite=Strict"
    );
    HeaderValue::from_str(&value).ok()
}

/// The `Set-Cookie` value that makes the browser forget it.
#[must_use]
pub fn clear() -> HeaderValue {
    HeaderValue::from_static(
        "__Secure-korpis_refresh=; Path=/korpis.v1.SessionService/; Max-Age=0; HttpOnly; Secure; \
         SameSite=Strict",
    )
}

/// The refresh credential the request's cookies carry, if one does.
pub fn read(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == REFRESH)
        .map(|(_, value)| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cookie_is_scoped_and_read_back_from_among_others() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let set = set("korpisrt_abc", now + Duration::from_secs(60), now).unwrap();
        let set = set.to_str().unwrap();
        for part in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/korpis.v1.SessionService/"] {
            assert!(set.contains(part), "{set}");
        }
        assert!(set.contains("Max-Age=60"));
        let mut headers = HeaderMap::new();
        let cookie = HeaderValue::from_static("theme=dark; __Secure-korpis_refresh=korpisrt_abc");
        headers.insert(http::header::COOKIE, cookie);
        assert_eq!(read(&headers).as_deref(), Some("korpisrt_abc"));
        assert_eq!(read(&HeaderMap::new()), None);
        assert!(clear().to_str().unwrap().contains("Max-Age=0"));
    }
}
