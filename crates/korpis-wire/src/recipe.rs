// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipe references: `korpis.v1.RecipeRef`.

use korpis_core::recipe::RecipeRef;
use korpis_proto::korpis::v1 as pb;

/// The wire form of a recipe reference.
#[must_use]
pub fn ref_to_pb(r: &RecipeRef) -> pb::RecipeRef {
    pb::RecipeRef {
        name: r.name.clone(),
        version: r.version.clone(),
        digest: r.digest.clone(),
        ..Default::default()
    }
}

/// A recipe reference as sent. Its form is checked with the body it belongs to, and an empty
/// digest is how a client asks for `name:version` to be resolved (§8 of docs/09-recipes.md).
#[must_use]
pub fn ref_from_pb(r: &pb::RecipeRef) -> RecipeRef {
    RecipeRef { name: r.name.clone(), version: r.version.clone(), digest: r.digest.clone() }
}
