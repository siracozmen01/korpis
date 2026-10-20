// SPDX-License-Identifier: AGPL-3.0-or-later

//! Korpis's first-party OpenID Connect identity provider (§5.3 and §9.7 of
//! docs/08-identity.md). The control plane never speaks OIDC; this does. It sends a person to
//! the issuer with the state, nonce, and PKCE challenge the control plane made, redeems the code
//! they return with and validates the ID token by OIDC Core §3.1.3.7, and answers re-verification
//! by refreshing each person's token, kept in its own volume, one person at a time. It is built
//! as a third party's extension would be, on the SDK alone (§1 of docs/22-first-party.md).

pub mod claims;
pub mod config;
pub mod issuer;
pub mod jwk;
pub mod jws;
pub mod provider;
pub mod refresh;

pub use provider::Oidc;
