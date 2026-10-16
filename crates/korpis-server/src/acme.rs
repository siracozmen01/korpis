// SPDX-License-Identifier: AGPL-3.0-or-later

//! The ACME directory the operator configures (§3.2 of docs/07-networking.md), Let's Encrypt by
//! default, and the installation's account at it. The account is made the first time a
//! certificate is ordered, never at start, so an installation that serves no ingress never
//! contacts a directory, and only if the operator agreed to the directory's terms; its
//! credentials are kept in the data directory beside the other keys, one file per directory.
//! Each order makes a fresh key for its certificate, which never leaves this process but sealed.

use crate::data::DataDir;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy, RevocationReason, RevocationRequest,
};
use korpis_app::ports::{
    Acme, AcmeError, AcmeFuture, Challenge, HttpOffer, Issued, Proof, Publish,
};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::OnceCell;
use zeroize::Zeroizing;

/// Let's Encrypt's production directory.
pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// How long an order may take to become ready once its challenges are, and its certificate to
/// be issued once it is finalized.
const ORDER_WITHIN: Duration = Duration::from_secs(120);

/// How the directory is reached, and as whom: Let's Encrypt by default, its terms not agreed.
#[derive(Debug, Clone)]
pub struct AcmeConfig {
    /// The directory's URL.
    pub directory: String,
    /// A certificate, in PEM, the directory's own is trusted by instead of the system's roots:
    /// for a test directory's private authority.
    pub ca: Option<PathBuf>,
    /// The account's contacts, `mailto:` URIs.
    pub contact: Vec<String>,
    /// Whether the operator agreed to the directory's terms.
    pub agreed: bool,
}

impl Default for AcmeConfig {
    fn default() -> Self {
        Self { directory: LETS_ENCRYPT.into(), ca: None, contact: Vec::new(), agreed: false }
    }
}

/// The directory, and the account once it is had.
pub struct AcmeClient {
    config: AcmeConfig,
    kept: PathBuf,
    account: OnceCell<Account>,
}

impl std::fmt::Debug for AcmeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcmeClient")
            .field("directory", &self.config.directory)
            .finish_non_exhaustive()
    }
}

fn unavailable(e: impl std::fmt::Display) -> AcmeError {
    AcmeError::Unavailable(e.to_string())
}

/// What `e` says of the directory: a problem it answered with is a refusal, anything else that it
/// could not be used.
fn judged(e: instant_acme::Error) -> AcmeError {
    match e {
        instant_acme::Error::Api(problem) => AcmeError::Refused(problem.to_string()),
        other => unavailable(other),
    }
}

impl AcmeClient {
    /// The directory `config` names, its account kept in `data`'s keys.
    #[must_use]
    pub fn new(config: AcmeConfig, data: &DataDir) -> Self {
        let digest = Sha256::digest(config.directory.as_bytes());
        let name = digest.iter().take(8).fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        let kept = data.keys().join(format!("acme-{name}.json"));
        Self { config, kept, account: OnceCell::new() }
    }

    fn builder(&self) -> Result<instant_acme::AccountBuilder, AcmeError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        match &self.config.ca {
            Some(ca) => Account::builder_with_root(ca).map_err(unavailable),
            None => Account::builder().map_err(unavailable),
        }
    }

    /// The account: the one kept for this directory, or a new one, kept, if the terms are
    /// agreed.
    async fn account(&self) -> Result<&Account, AcmeError> {
        self.account.get_or_try_init(|| async { self.open_or_create().await }).await
    }

    async fn open_or_create(&self) -> Result<Account, AcmeError> {
        if let Ok(bytes) = std::fs::read(&self.kept) {
            let credentials: AccountCredentials =
                serde_json::from_slice(&bytes).map_err(unavailable)?;
            return self.builder()?.from_credentials(credentials).await.map_err(judged);
        }
        if !self.config.agreed {
            let why = format!("{} (korpis-server serve --acme-agree-terms)", self.config.directory);
            return Err(AcmeError::Terms(why));
        }
        let contact: Vec<&str> = self.config.contact.iter().map(String::as_str).collect();
        let new = NewAccount {
            contact: &contact,
            terms_of_service_agreed: true,
            only_return_existing: false,
        };
        let (account, credentials) = self
            .builder()?
            .create(&new, self.config.directory.clone(), None)
            .await
            .map_err(judged)?;
        let bytes = serde_json::to_vec(&credentials).map_err(unavailable)?;
        crate::data::write_secret(&self.kept, &bytes).map_err(unavailable)?;
        Ok(account)
    }
}

/// The challenges of `kind` offered for each name of `order` not yet valid, each read by
/// `offer`: the authorizations are read once per kind, since a handle gives one challenge.
async fn offered<T>(
    order: &mut instant_acme::Order,
    kind: ChallengeType,
    offer: impl Fn(&instant_acme::ChallengeHandle<'_>) -> T,
) -> Result<Vec<(String, Option<T>)>, AcmeError> {
    let mut out = Vec::new();
    let mut authorizations = order.authorizations();
    while let Some(next) = authorizations.next().await {
        let mut authorization = next.map_err(judged)?;
        match authorization.status {
            AuthorizationStatus::Valid => continue,
            AuthorizationStatus::Pending => {}
            other => return Err(AcmeError::Refused(format!("an authorization is {other:?}"))),
        }
        let name = authorization.identifier().to_string();
        out.push((name, authorization.challenge(kind.clone()).as_ref().map(&offer)));
    }
    Ok(out)
}

/// The challenges offered for each name of `order` not yet valid.
async fn challenges(order: &mut instant_acme::Order) -> Result<Vec<Challenge>, AcmeError> {
    let http = offered(order, ChallengeType::Http01, |c| HttpOffer {
        token: c.token.clone(),
        key_authorization: c.key_authorization().as_str().to_owned(),
    })
    .await?;
    let dns: BTreeMap<String, Option<String>> =
        offered(order, ChallengeType::Dns01, |c| c.key_authorization().dns_value())
            .await?
            .into_iter()
            .collect();
    Ok(http
        .into_iter()
        .map(|(name, http)| {
            let dns_value = dns.get(&name).cloned().flatten();
            Challenge { name, http, dns_value }
        })
        .collect())
}

/// Tells the directory each pending name's challenge is ready, of the kind `proofs` says it was
/// published by.
async fn ready(
    order: &mut instant_acme::Order,
    proofs: &BTreeMap<String, Proof>,
) -> Result<(), AcmeError> {
    let mut authorizations = order.authorizations();
    while let Some(next) = authorizations.next().await {
        let mut authorization = next.map_err(judged)?;
        if authorization.status != AuthorizationStatus::Pending {
            continue;
        }
        let name = authorization.identifier().to_string();
        let kind = match proofs.get(&name) {
            Some(Proof::Http01) => ChallengeType::Http01,
            Some(Proof::Dns01) => ChallengeType::Dns01,
            None => return Err(AcmeError::Refused(format!("{name} was not published"))),
        };
        if let Some(mut challenge) = authorization.challenge(kind) {
            challenge.set_ready().await.map_err(judged)?;
        }
    }
    Ok(())
}

impl Acme for AcmeClient {
    fn issue<'a>(&'a self, names: &'a [String], publish: Publish<'a>) -> AcmeFuture<'a, Issued> {
        Box::pin(async move {
            let account = self.account().await?;
            let identifiers: Vec<Identifier> =
                names.iter().map(|n| Identifier::Dns(n.clone())).collect();
            let mut order =
                account.new_order(&NewOrder::new(&identifiers)).await.map_err(judged)?;
            let pending = challenges(&mut order).await?;
            let pending_names: Vec<String> = pending.iter().map(|c| c.name.clone()).collect();
            let proofs = publish(pending).await?;
            ready(&mut order, &pending_names.into_iter().zip(proofs).collect()).await?;
            let within = RetryPolicy::default().timeout(ORDER_WITHIN);
            let status = order.poll_ready(&within).await.map_err(judged)?;
            if status != OrderStatus::Ready {
                return Err(AcmeError::Refused(format!("the order is {status:?}")));
            }
            let key = Zeroizing::new(order.finalize().await.map_err(judged)?);
            let chain = order.poll_certificate(&within).await.map_err(judged)?;
            Ok(Issued { chain, key })
        })
    }

    fn revoke<'a>(&'a self, chain: &'a str) -> AcmeFuture<'a, ()> {
        Box::pin(async move {
            let leaf = CertificateDer::pem_slice_iter(chain.as_bytes())
                .next()
                .ok_or_else(|| AcmeError::Refused("the chain holds no certificate".into()))?
                .map_err(|e| AcmeError::Refused(e.to_string()))?;
            let request = RevocationRequest {
                certificate: &leaf,
                reason: Some(RevocationReason::KeyCompromise),
            };
            self.account().await?.revoke(&request).await.map_err(judged)
        })
    }
}
