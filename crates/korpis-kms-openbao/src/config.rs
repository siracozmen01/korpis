// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider's configuration, from its workload's environment (§2 of docs/16-extensions.md:
//! the service's own credential lives in the extension's configuration and nowhere else).

use std::collections::BTreeMap;

/// What the provider is configured with.
#[derive(Debug)]
pub struct Config {
    /// The engine's address.
    pub address: url::Url,
    /// Its Transit mount.
    pub mount: String,
    /// Its namespace, where it has them.
    pub namespace: Option<String>,
    /// The provider's token, which the tenant's policy limits to its keys' datakey and decrypt.
    pub token: String,
    /// Which of its installation's `provides` entries it registers as.
    pub name: String,
    /// How many calls it takes at once.
    pub concurrency: u32,
}

/// The variables read, and what each is.
pub const VARIABLES: &[(&str, &str)] = &[
    ("KORPIS_OPENBAO_ADDR", "the engine's address, https unless on this machine"),
    ("KORPIS_OPENBAO_MOUNT", "its Transit mount; 'transit' when not set"),
    ("KORPIS_OPENBAO_NAMESPACE", "its namespace, where it has them"),
    ("KORPIS_OPENBAO_TOKEN", "the provider's token"),
    ("KORPIS_PROVIDER_NAME", "the `provides` entry it registers as; 'openbao' when not set"),
    ("KORPIS_PROVIDER_CONCURRENCY", "calls at once, 1 to 1024; 16 when not set"),
];

/// Reads the configuration from `env`.
///
/// # Errors
/// A variable that is required is missing, or one is not what it must be.
pub fn from(env: &BTreeMap<String, String>) -> Result<Config, String> {
    let get = |k: &str| env.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let need = |k: &str| get(k).ok_or_else(|| format!("{k} is not set"));
    let address: url::Url =
        need("KORPIS_OPENBAO_ADDR")?.parse().map_err(|e| format!("KORPIS_OPENBAO_ADDR: {e}"))?;
    let local = address.host_str().is_some_and(|h| h == "localhost" || h.starts_with("127."));
    if address.scheme() != "https" && !local {
        return Err(
            "KORPIS_OPENBAO_ADDR: a service on another machine is reached over https".into()
        );
    }
    let concurrency = match get("KORPIS_PROVIDER_CONCURRENCY") {
        None => 16,
        Some(v) => v
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=1024).contains(n))
            .ok_or("KORPIS_PROVIDER_CONCURRENCY: 1 to 1024")?,
    };
    Ok(Config {
        address,
        mount: get("KORPIS_OPENBAO_MOUNT").unwrap_or("transit").to_owned(),
        namespace: get("KORPIS_OPENBAO_NAMESPACE").map(str::to_owned),
        token: need("KORPIS_OPENBAO_TOKEN")?.to_owned(),
        name: get("KORPIS_PROVIDER_NAME").unwrap_or("openbao").to_owned(),
        concurrency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn a_remote_service_is_reached_over_https_and_the_rest_defaults() {
        let remote =
            env(&[("KORPIS_OPENBAO_ADDR", "http://kms.example"), ("KORPIS_OPENBAO_TOKEN", "t")]);
        assert!(from(&remote).is_err());
        let local =
            env(&[("KORPIS_OPENBAO_ADDR", "http://127.0.0.1:8200"), ("KORPIS_OPENBAO_TOKEN", "t")]);
        let c = from(&local).unwrap();
        assert_eq!((c.mount.as_str(), c.name.as_str(), c.concurrency), ("transit", "openbao", 16));
        assert!(from(&env(&[("KORPIS_OPENBAO_ADDR", "https://kms.example")])).is_err(), "a token");
    }
}
