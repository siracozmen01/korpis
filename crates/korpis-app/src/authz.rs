// SPDX-License-Identifier: AGPL-3.0-or-later

//! Authority: authenticating callers, the one authorization path every use case goes through
//! (P6), and the grant use cases of docs/08-identity.md.

mod approve;
mod attest;
mod denial;
mod failure;
pub(crate) mod gateway;
mod issue;
mod mint;
mod query;
mod revocation;
mod revoke;
mod snapshot;
mod template;

pub use issue::{Holder, IssueGrant, Issued, MAX_LABEL};
pub use mint::{MintToken, Minted};
pub use query::{GrantWithChain, GrantsFilter};
pub use revoke::Revoked;
pub(crate) use snapshot::Snapshot;
pub use template::{TemplateAsked, TemplatesOf};
