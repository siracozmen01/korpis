// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipes as OCI artifacts (§2 and §3 of docs/09-recipes.md): the document format, the layout
//! of the artifact in a registry, and the registry client that resolves and fetches them. This
//! is an adapter: it implements the recipe source port of `korpis-app`, and the CLI uses the
//! same code to validate a declaration before it is sent. Without its `source` feature it is
//! the document format and the artifact layout alone, which is all a runtime driver reads.

#![warn(missing_docs)]

pub mod artifact;
pub mod document;
#[cfg(feature = "source")]
pub mod registry;
#[cfg(feature = "source")]
pub mod source;

pub use document::{DocumentError, parse};
