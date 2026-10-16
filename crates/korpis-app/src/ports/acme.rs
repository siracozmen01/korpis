// SPDX-License-Identifier: AGPL-3.0-or-later

//! Obtaining certificates from an ACME directory (§3.2 of docs/07-networking.md, RFC 8555), with
//! the installation's account there. One call runs a whole order: the directory's challenges are
//! handed to the caller to publish, and the order goes on once they are published, so whatever
//! holds an order's state stays in the adapter.

use std::future::Future;
use std::pin::Pin;

/// A future the ACME port answers with.
pub type AcmeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, AcmeError>> + Send + 'a>>;

/// What publishes an order's challenges, answering once each is answered where the directory
/// will look for it.
pub type Publish<'a> = Box<dyn FnOnce(Vec<Challenge>) -> AcmeFuture<'a, ()> + Send + 'a>;

/// How control of a name is proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proof {
    /// By serving the key authorization at `/.well-known/acme-challenge/<token>` on port 80 of
    /// the name.
    Http01,
    /// By publishing a TXT record at `_acme-challenge.<name>`.
    Dns01,
}

/// One name's challenge.
#[derive(Clone, PartialEq, Eq)]
pub struct Challenge {
    /// The name it proves control of.
    pub name: String,
    /// The token, for HTTP-01 the last segment of the path it is answered at.
    pub token: String,
    /// For HTTP-01: the body answered.
    pub key_authorization: String,
    /// For DNS-01: the TXT record's value.
    pub dns_value: String,
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
    /// Orders a certificate for `names`, proving control of each by `proof`: the challenges are
    /// handed to `publish`, and the directory told to look once it answers.
    fn issue<'a>(
        &'a self,
        names: &'a [String],
        proof: Proof,
        publish: Publish<'a>,
    ) -> AcmeFuture<'a, Issued>;
}
