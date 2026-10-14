// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider's configuration, from its workload's environment (§2 of docs/16-extensions.md:
//! the store's own credential lives in the extension's configuration and nowhere else).

use crate::sts::Sts;
use crate::target::Place;
use std::collections::BTreeMap;

/// What the provider is configured with.
#[derive(Debug)]
pub struct Config {
    /// Where it keeps backups, with its own credential.
    pub place: Place,
    /// Its store's token service.
    pub sts: Sts,
    /// Which of its installation's `provides` entries it registers as.
    pub name: String,
    /// How many calls it takes at once.
    pub concurrency: u32,
}

/// The variables read, and what each is.
pub const VARIABLES: &[(&str, &str)] = &[
    ("KORPIS_S3_ENDPOINT", "the store's address, https unless on this machine"),
    ("KORPIS_S3_REGION", "its region; us-east-1 when not set"),
    ("KORPIS_S3_BUCKET", "the bucket"),
    ("KORPIS_S3_ROOT", "the provider's place in the bucket: empty, or a prefix ending in '/'"),
    ("KORPIS_S3_PATH_STYLE", "'true' to address the bucket in the path, as self-hosted stores do"),
    ("KORPIS_S3_ACCESS_KEY_ID", "the provider's own access key"),
    ("KORPIS_S3_SECRET_ACCESS_KEY", "its secret"),
    ("KORPIS_S3_STS_ENDPOINT", "the token service's address; the store's when not set"),
    ("KORPIS_S3_ROLE_ARN", "the role each job's credential assumes, where the service needs one"),
    ("KORPIS_PROVIDER_NAME", "the `provides` entry it registers as; 's3' when not set"),
    ("KORPIS_PROVIDER_CONCURRENCY", "calls at once, 1 to 1024; 16 when not set"),
];

/// Reads the configuration from `env`.
///
/// # Errors
/// A variable that is required is missing, or one is not what it must be.
pub fn from(env: &BTreeMap<String, String>, http: reqwest::Client) -> Result<Config, String> {
    let get = |k: &str| env.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let need = |k: &str| get(k).ok_or_else(|| format!("{k} is not set"));
    let url = |k: &str, v: &str| v.parse::<url::Url>().map_err(|e| format!("{k}: {e}"));
    let endpoint = url("KORPIS_S3_ENDPOINT", need("KORPIS_S3_ENDPOINT")?)?;
    let local = endpoint.host_str().is_some_and(|h| h == "localhost" || h.starts_with("127."));
    if endpoint.scheme() != "https" && !local {
        return Err("KORPIS_S3_ENDPOINT: a store on another machine is reached over https".into());
    }
    let region = get("KORPIS_S3_REGION").unwrap_or("us-east-1").to_owned();
    let (access_key_id, secret) = (
        need("KORPIS_S3_ACCESS_KEY_ID")?.to_owned(),
        need("KORPIS_S3_SECRET_ACCESS_KEY")?.to_owned(),
    );
    let sts_endpoint = match get("KORPIS_S3_STS_ENDPOINT") {
        Some(v) => url("KORPIS_S3_STS_ENDPOINT", v)?,
        None => endpoint.clone(),
    };
    let concurrency = match get("KORPIS_PROVIDER_CONCURRENCY") {
        Some(v) => v
            .parse()
            .ok()
            .filter(|n| (1..=1024).contains(n))
            .ok_or("KORPIS_PROVIDER_CONCURRENCY: 1 to 1024")?,
        None => 16,
    };
    let sts = Sts {
        endpoint: sts_endpoint,
        region: region.clone(),
        access_key_id: access_key_id.clone(),
        secret: secret.clone(),
        role_arn: get("KORPIS_S3_ROLE_ARN").map(str::to_owned),
        http,
    };
    let place = Place {
        endpoint,
        region,
        bucket: need("KORPIS_S3_BUCKET")?.to_owned(),
        root: get("KORPIS_S3_ROOT").unwrap_or_default().to_owned(),
        path_style: get("KORPIS_S3_PATH_STYLE") == Some("true"),
        access_key_id,
        secret,
    };
    let name = get("KORPIS_PROVIDER_NAME").unwrap_or("s3").to_owned();
    Ok(Config { place, sts, name, concurrency })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http() -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::new()
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn a_configuration_is_read_with_its_defaults_and_refused_without_what_it_needs() {
        let base = [
            ("KORPIS_S3_ENDPOINT", "https://s3.eu-central-1.amazonaws.com"),
            ("KORPIS_S3_BUCKET", "backups"),
            ("KORPIS_S3_ACCESS_KEY_ID", "AKIA"),
            ("KORPIS_S3_SECRET_ACCESS_KEY", "secret"),
            ("KORPIS_S3_STS_ENDPOINT", "https://sts.eu-central-1.amazonaws.com"),
        ];
        let c = from(&env(&base), http()).unwrap();
        assert_eq!((c.name.as_str(), c.concurrency, c.place.root.as_str()), ("s3", 16, ""));
        assert_eq!(c.sts.endpoint.as_str(), "https://sts.eu-central-1.amazonaws.com/");
        assert!(!format!("{c:?}").contains("secret"), "the secret is never shown");
        let e = from(&env(&base[1..]), http()).unwrap_err();
        assert!(e.contains("KORPIS_S3_ENDPOINT"), "{e}");
        let mut plain = base.to_vec();
        plain[0] = ("KORPIS_S3_ENDPOINT", "http://s3.example");
        assert!(from(&env(&plain), http()).is_err(), "plain http elsewhere");
    }
}
