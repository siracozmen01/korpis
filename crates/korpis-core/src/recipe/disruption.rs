// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a recipe says interruption costs (§5.1 of docs/09-recipes.md). It may only make a step
//! more disruptive than the driver says, never less.

/// One declaration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisruptionDecl {
    /// Whether the operation is disruptive for this software.
    pub disruptive: bool,
    /// Why, for the plan's reader.
    pub reason: String,
}

/// The recipe's disruption block. Absent declarations inherit the driver's classification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Disruption {
    /// Stopping.
    pub stop: Option<DisruptionDecl>,
    /// Restarting.
    pub restart: Option<DisruptionDecl>,
    /// Configuration keys whose change makes a reconfiguration disruptive.
    pub disruptive_fields: Vec<String>,
}
