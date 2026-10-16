// SPDX-License-Identifier: AGPL-3.0-or-later

//! Obtaining certificates from an ACME directory (§3.2 of docs/07-networking.md, RFC 8555), with
//! the installation's account there. One call runs a whole order: the challenges the directory
//! offers for each name are handed to the caller, which publishes one of each name's and says
//! which, and the order goes on once they are published, so whatever holds an order's state stays
//! in the adapter. Each name is proven its own way, so one certificate may cover names of a zone a
//! DNS provider serves and names only a node's proxy answers for.

use std::future::Future;
use std::pin::Pin;

/// A future the ACME port answers with.
pub type AcmeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, AcmeError>> + Send + 'a>>;

/// What publishes an order's challenges, one of each name's, answering, once each is answered
/// where the directory will look for it, with how each name is proven, in the order given.
pub type Publish<'a> = Box<dyn FnOnce(Vec<Challenge>) -> AcmeFuture<'a, Vec<Proof>> + Send + 'a>;

/// How control of a name is proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proof {
    /// By serving the key authorization at `/.well-known/acme-challenge/<token>` on port 80 of
    /// the name.
    Http01,
    /// By publishing a TXT record at `_acme-challenge.<name>`.
    Dns01,
}

/// The challenges the directory offers for one name.
#[derive(Clone, PartialEq, Eq)]
pub struct Challenge {
    /// The name they prove control of.
    pub name: String,
    /// HTTP-01, if offered.
    pub http: Option<HttpOffer>,
    /// DNS-01, if offered: the TXT record's value.
    pub dns_value: Option<String>,
}

/// An HTTP-01 challenge.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpOffer {
    /// The token, the last segment of the path it is answered at.
    pub token: String,
    /// The body answered.
    pub key_authorization: String,
}

impl std::fmt::Debug for HttpOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpOffer").field("token", &self.token).finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Challenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Challenge").field("name", &self.name).finish_non_exhaustive()
    }
}

/// A certificate issued: its chain, leaf first, and the key it was issued for, in PEM.
#[derive(Clone)]
pub struct Issued {
    /// The chain.
    pub chain: String,
    /// The leaf's private key, made for this order alone.
    pub key: zeroize::Zeroizing<String>,
}

impl std::fmt::Debug for Issued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Issued").finish_non_exhaustive()
    }
}

/// Why an order gave no certificate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcmeError {
    /// The directory refused: a name it will not issue for, a challenge that failed, a limit.
    #[error("the ACME directory refused: {0}")]
    Refused(String),
    /// The directory, or whatever publishes the challenges, could not be reached.
    #[error("the ACME directory could not be used: {0}")]
    Unavailable(String),
    /// The installation has not agreed to the directory's terms, without which it makes no
    /// account there.
    #[error("the ACME directory's terms are not agreed: {0}")]
    Terms(String),
}

/// An ACME directory, with the installation's account at it.
pub trait Acme: Send + Sync + std::fmt::Debug {
    /// Orders a certificate for `names`: the challenges offered are handed to `publish`, and the
    /// directory told to look at those it says it published once it answers.
    fn issue<'a>(&'a self, names: &'a [String], publish: Publish<'a>) -> AcmeFuture<'a, Issued>;

    /// Revokes the certificate `chain` leads with, its key having been exposed
    /// (`keyCompromise`, RFC 5280 §5.3.1).
    fn revoke<'a>(&'a self, chain: &'a str) -> AcmeFuture<'a, ()>;
}
