// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `WebAuthn` relying party (§9.6 of docs/08-identity.md): the site passkeys are bound to,
//! and the origins a ceremony may come from. The installation is reached at `--public-url`; its
//! host is the relying party's identifier unless `--rp-id` names a registrable suffix of it, so a
//! panel at a sibling name of the same site (§1 of docs/11-surface-web.md) can share it; and the
//! origins are the public URL's and those `--web-origin` lists, each of which must be inside the
//! identifier, since a browser refuses a ceremony for a site its page is not part of.

use http::Uri;
use korpis_core::webauthn::RelyingParty;

/// An origin and its host, from `url`: `https://` anywhere, `http://` only for `localhost`,
/// which browsers treat as secure, and nothing after the authority.
fn origin(url: &str) -> Result<(String, String), String> {
    let uri: Uri = url.parse().map_err(|e| format!("{url}: {e}"))?;
    let host = uri.host().ok_or_else(|| format!("{url}: no host"))?.to_ascii_lowercase();
    let secure = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => host == "localhost",
        _ => false,
    };
    if !secure {
        return Err(format!("{url}: a ceremony needs https, or http on localhost"));
    }
    if uri.path_and_query().is_some_and(|p| p.as_str() != "/" && !p.as_str().is_empty()) {
        return Err(format!("{url}: an origin has no path"));
    }
    let authority = uri.authority().map(|a| a.as_str().to_ascii_lowercase()).unwrap_or_default();
    let scheme = uri.scheme_str().unwrap_or_default();
    Ok((format!("{scheme}://{authority}"), host))
}

/// Whether `host` is `site` or a name beneath it.
fn within(host: &str, site: &str) -> bool {
    host == site || host.strip_suffix(site).is_some_and(|rest| rest.ends_with('.'))
}

/// The relying party of an installation reached at `public_url`, identified as `rp_id` if given,
/// serving panels at `web_origins` besides. `None` without a public URL: passkeys then cannot be
/// registered or used, and the control plane says so at start.
pub fn relying_party(
    public_url: Option<&str>,
    rp_id: Option<&str>,
    web_origins: &[String],
) -> Result<Option<RelyingParty>, String> {
    let Some(public_url) = public_url else {
        return match (rp_id, web_origins.is_empty()) {
            (None, true) => Ok(None),
            _ => Err("--rp-id and --web-origin need --public-url".into()),
        };
    };
    let (own, host) = origin(public_url)?;
    let id = rp_id.map_or_else(|| host.clone(), str::to_ascii_lowercase);
    let mut origins = vec![own];
    for url in web_origins {
        let (o, h) = origin(url)?;
        if !within(&h, &id) {
            return Err(format!("{url}: not within the relying party {id}"));
        }
        origins.push(o);
    }
    if !within(&host, &id) || !id.contains('.') && id != "localhost" {
        return Err(format!("--rp-id {id}: not a registrable suffix of {host}"));
    }
    origins.dedup();
    Ok(Some(RelyingParty { id, origins }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_url_s_host_is_the_site_unless_a_suffix_of_it_is_named() {
        let rp = relying_party(Some("https://Korpis.Example.com:7443"), None, &[]).unwrap();
        let rp = rp.unwrap();
        assert_eq!(rp.id, "korpis.example.com");
        assert_eq!(rp.origins, ["https://korpis.example.com:7443"]);

        let panel = ["https://panel.example.com".to_owned()];
        let rp = relying_party(Some("https://api.example.com"), Some("example.com"), &panel);
        assert_eq!(rp.unwrap().unwrap().origins.len(), 2);
        let local = relying_party(Some("http://localhost:7443"), None, &[]).unwrap().unwrap();
        assert_eq!(local.id, "localhost");
        assert_eq!(relying_party(None, None, &[]), Ok(None));
    }

    #[test]
    fn what_a_browser_would_refuse_is_refused_at_start() {
        let other = ["https://panel.elsewhere.net".to_owned()];
        assert!(relying_party(Some("https://api.example.com"), None, &other).is_err());
        assert!(relying_party(Some("http://api.example.com"), None, &[]).is_err());
        assert!(relying_party(Some("https://api.example.com/panel"), None, &[]).is_err());
        assert!(relying_party(Some("https://api.example.com"), Some("ample.com"), &[]).is_err());
        assert!(relying_party(Some("https://api.example.com"), Some("com"), &[]).is_err());
        assert!(relying_party(None, Some("example.com"), &[]).is_err());
    }
}
