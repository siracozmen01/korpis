// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider's configuration, from its workload's environment (§5.3 of docs/08-identity.md):
//! the issuer, the client registered there and its secret, which lives in the extension's
//! configuration and nowhere else, the claim naming groups, and whether to ask for offline
//! access, which needs somewhere of the provider's own to keep refresh tokens.

use std::collections::BTreeMap;
use std::path::PathBuf;
use url::Url;

/// What the provider is configured with.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The issuer, exactly as configured, which is how its tokens must name it: kept as text,
    /// since a URL's normal form may add a `/` the issuer's `iss` does not have.
    pub issuer: String,
    /// The client registered at the issuer.
    pub client_id: String,
    /// Its secret.
    pub client_secret: String,
    /// The claim naming a person's groups, the realm of every set this provider vouches for.
    pub groups_claim: String,
    /// The scopes asked for: `openid`, those configured, and `offline_access` when asked.
    pub scopes: Vec<String>,
    /// Where refresh tokens are kept, when offline access is asked for.
    pub state_dir: Option<PathBuf>,
}

/// What the provider is configured with, beside its settings.
#[derive(Debug, Clone)]
pub struct Config {
    /// What it speaks to the issuer with.
    pub settings: Settings,
    /// Which of its installation's `provides` entries it registers as.
    pub name: String,
    /// How many calls it takes at once.
    pub concurrency: u32,
}

/// The variables read, and what each is.
pub const VARIABLES: &[(&str, &str)] = &[
    ("KORPIS_OIDC_ISSUER", "the issuer, https unless on this machine, exactly as its tokens say"),
    ("KORPIS_OIDC_CLIENT_ID", "the client registered at the issuer"),
    ("KORPIS_OIDC_CLIENT_SECRET", "its secret"),
    ("KORPIS_OIDC_GROUPS_CLAIM", "the claim naming a person's groups; 'groups' when not set"),
    ("KORPIS_OIDC_SCOPES", "scopes beyond 'openid', separated by spaces"),
    ("KORPIS_OIDC_OFFLINE", "'true' to ask for refresh tokens and re-verify people with them"),
    ("KORPIS_OIDC_STATE_DIR", "where refresh tokens are kept: the provider's own volume"),
    ("KORPIS_PROVIDER_NAME", "the `provides` entry it registers as; 'oidc' when not set"),
    ("KORPIS_PROVIDER_CONCURRENCY", "calls at once, 1 to 1024; 16 when not set"),
];

/// Whether `url` is https, or on this machine.
#[must_use]
pub fn secure(url: &Url) -> bool {
    let local = matches!(url.host_str(), Some("localhost" | "[::1]"))
        || url.host_str().is_some_and(|h| h.starts_with("127."));
    url.scheme() == "https" || (url.scheme() == "http" && local)
}

/// Reads the configuration from `env`.
///
/// # Errors
/// A variable that is required is missing, or one is not what it must be.
pub fn from(env: &BTreeMap<String, String>) -> Result<Config, String> {
    let get = |k: &str| env.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let need = |k: &str| get(k).ok_or_else(|| format!("{k} is not set"));
    let issuer = need("KORPIS_OIDC_ISSUER")?.to_owned();
    let parsed: Url = issuer.parse().map_err(|e| format!("KORPIS_OIDC_ISSUER: {e}"))?;
    if !secure(&parsed) || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(
            "KORPIS_OIDC_ISSUER: https unless on this machine, with no query or fragment".into()
        );
    }
    let groups_claim = get("KORPIS_OIDC_GROUPS_CLAIM").unwrap_or("groups").to_owned();
    if groups_claim.contains(':') {
        return Err("KORPIS_OIDC_GROUPS_CLAIM: a set's realm never holds a ':'".into());
    }
    let offline = get("KORPIS_OIDC_OFFLINE") == Some("true");
    let state_dir = get("KORPIS_OIDC_STATE_DIR").map(PathBuf::from);
    if offline && state_dir.is_none() {
        return Err("KORPIS_OIDC_STATE_DIR: offline access needs somewhere to keep tokens".into());
    }
    let mut scopes = vec!["openid".to_owned()];
    scopes.extend(
        get("KORPIS_OIDC_SCOPES").unwrap_or_default().split_whitespace().map(str::to_owned),
    );
    if offline {
        scopes.push("offline_access".into());
    }
    scopes.dedup();
    let concurrency = match get("KORPIS_PROVIDER_CONCURRENCY") {
        Some(v) => v
            .parse()
            .ok()
            .filter(|n| (1..=1024).contains(n))
            .ok_or("KORPIS_PROVIDER_CONCURRENCY: 1 to 1024")?,
        None => 16,
    };
    let settings = Settings {
        issuer,
        client_id: need("KORPIS_OIDC_CLIENT_ID")?.to_owned(),
        client_secret: need("KORPIS_OIDC_CLIENT_SECRET")?.to_owned(),
        groups_claim,
        scopes,
        state_dir: if offline { state_dir } else { None },
    };
    let name = get("KORPIS_PROVIDER_NAME").unwrap_or("oidc").to_owned();
    Ok(Config { settings, name, concurrency })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn a_configuration_is_read_with_its_defaults_and_refused_without_what_it_needs() {
        let base = [
            ("KORPIS_OIDC_ISSUER", "https://idp.example"),
            ("KORPIS_OIDC_CLIENT_ID", "korpis"),
            ("KORPIS_OIDC_CLIENT_SECRET", "s"),
        ];
        let c = from(&env(&base)).unwrap();
        assert_eq!((c.name.as_str(), c.concurrency), ("oidc", 16));
        assert_eq!(c.settings.groups_claim, "groups");
        assert_eq!(c.settings.scopes, ["openid"]);
        let offline = [&base[..], &[("KORPIS_OIDC_OFFLINE", "true")]].concat();
        assert!(from(&env(&offline)).is_err(), "offline access needs a state directory");
        let kept = [&offline[..], &[("KORPIS_OIDC_STATE_DIR", "/data")]].concat();
        assert_eq!(from(&env(&kept)).unwrap().settings.scopes, ["openid", "offline_access"]);
        let plain = [("KORPIS_OIDC_ISSUER", "http://idp.example"), base[1], base[2]];
        assert!(from(&env(&plain)).is_err(), "https unless on this machine");
        let local = [("KORPIS_OIDC_ISSUER", "http://127.0.0.1:4000"), base[1], base[2]];
        assert!(from(&env(&local)).is_ok());
        let colon = [&base[..], &[("KORPIS_OIDC_GROUPS_CLAIM", "a:b")]].concat();
        assert!(from(&env(&colon)).is_err());
    }
}
