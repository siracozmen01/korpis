// SPDX-License-Identifier: AGPL-3.0-or-later

//! Steps, the action vocabulary, and where `disruptive` comes from (§3.3 of docs/01-model.md,
//! finding R9): one override chain, every layer visible, and no layer below the operator able to
//! make a step less disruptive than the layer beneath it.

use crate::object::ObjectRef;
use crate::recipe::Disruption;
use crate::value::ValueMap;
use std::time::Duration;

/// The action vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepAction {
    /// Fetch the recipe's artifacts onto the node.
    PullRecipe,
    /// Create a volume.
    CreateVolume,
    /// Attach an existing volume.
    AttachVolume,
    /// Detach a volume; its data is retained.
    DetachVolume,
    /// Delete a volume and its data.
    DeleteVolume,
    /// Create the runtime instance.
    Create,
    /// Start it.
    Start,
    /// Stop it.
    Stop,
    /// Restart it once.
    Restart,
    /// Apply a changed configuration.
    Reconfigure,
    /// Apply changed endpoints and network policy.
    ReconfigureNetwork,
    /// Release an address back to its pool.
    ReleaseAddress,
    /// Move to another node.
    Migrate,
    /// Remove the runtime instance; volumes are retained.
    Retire,
}

impl StepAction {
    /// The wire and stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PullRecipe => "pull_recipe",
            Self::CreateVolume => "create_volume",
            Self::AttachVolume => "attach_volume",
            Self::DetachVolume => "detach_volume",
            Self::DeleteVolume => "delete_volume",
            Self::Create => "create",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Reconfigure => "reconfigure",
            Self::ReconfigureNetwork => "reconfigure_network",
            Self::ReleaseAddress => "release_address",
            Self::Migrate => "migrate",
            Self::Retire => "retire",
        }
    }

    const ALL: [Self; 14] = [
        Self::PullRecipe,
        Self::CreateVolume,
        Self::AttachVolume,
        Self::DetachVolume,
        Self::DeleteVolume,
        Self::Create,
        Self::Start,
        Self::Stop,
        Self::Restart,
        Self::Reconfigure,
        Self::ReconfigureNetwork,
        Self::ReleaseAddress,
        Self::Migrate,
        Self::Retire,
    ];

    /// The action a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.as_str() == s)
    }

    /// The vocabulary's floor: (disruptive, reversible). Nothing relaxes the disruptive floor,
    /// and reversibility is a property of the action alone.
    #[must_use]
    pub const fn floor(self) -> (bool, bool) {
        match self {
            Self::DeleteVolume | Self::ReleaseAddress => (true, false),
            Self::Migrate | Self::Retire => (true, true),
            _ => (false, true),
        }
    }
}

/// Who executes a step (§4.9 of docs/02-architecture.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepExecutor {
    /// The control plane.
    ControlPlane,
    /// The node agent.
    Node,
    /// An extension.
    Extension,
}

/// The layer that decided a step's classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// The action vocabulary's floor.
    Vocabulary,
    /// The runtime driver.
    Driver,
    /// The recipe's `disruption` block.
    Recipe,
    /// The operator's recorded override for this workload.
    Operator,
}

/// How a runtime driver changes the floor (§3.3 of docs/01-model.md, docs/04-runtimes.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriverClass {
    /// Actions the driver makes disruptive on its tier, e.g. `stop` on a `vm` costs a boot.
    pub disruptive: Vec<StepAction>,
}

/// An operator's override of one action's classification for one workload. Recorded as an
/// effect when it is set; it may relax the driver and the recipe, never the vocabulary's floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Override {
    /// The action.
    pub action: StepAction,
    /// Disruptive or not.
    pub disruptive: bool,
}

/// Everything a classification reads.
#[derive(Debug, Clone, Copy)]
pub struct Classifier<'a> {
    /// The driver's layer.
    pub driver: &'a DriverClass,
    /// The recipe's layer.
    pub recipe: &'a Disruption,
    /// The operator's overrides for this workload.
    pub overrides: &'a [Override],
}

impl Classifier<'_> {
    /// Classifies `action`. `touched_fields` are the configuration keys a reconfiguration
    /// changes, which the recipe's `disruptive_fields` may make disruptive.
    #[must_use]
    pub fn classify(&self, action: StepAction, touched_fields: &[String]) -> (bool, Layer, String) {
        let (floor, _) = action.floor();
        let mut verdict =
            (floor, Layer::Vocabulary, format!("{} by the vocabulary", action.as_str()));
        if !floor && self.driver.disruptive.contains(&action) {
            verdict = (true, Layer::Driver, "disruptive on this driver's tier".into());
        }
        if !verdict.0 {
            let decl = match action {
                StepAction::Stop => self.recipe.stop.as_ref(),
                StepAction::Restart | StepAction::Retire => self.recipe.restart.as_ref(),
                _ => None,
            };
            if let Some(d) = decl.filter(|d| d.disruptive) {
                verdict = (true, Layer::Recipe, d.reason.clone());
            } else if action == StepAction::Reconfigure
                && let Some(f) =
                    touched_fields.iter().find(|f| self.recipe.disruptive_fields.contains(f))
            {
                verdict = (true, Layer::Recipe, format!("the recipe declares {f} disruptive"));
            }
        }
        if let Some(o) = self.overrides.iter().find(|o| o.action == action)
            && (o.disruptive || !floor)
        {
            verdict =
                (o.disruptive, Layer::Operator, "set by the operator for this workload".into());
        }
        verdict
    }
}

/// One step of a plan, keyed by `(plan, index)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// Its position; applying a completed step is a no-op that reports success.
    pub index: u32,
    /// What it does.
    pub action: StepAction,
    /// Who executes it.
    pub executor: StepExecutor,
    /// What it acts on.
    pub target: ObjectRef,
    /// Whether it can be undone.
    pub reversible: bool,
    /// Resolved through the override chain, never inferred.
    pub disruptive: bool,
    /// The layer that decided `disruptive`, and why, for the plan's reader.
    pub classified_by: Layer,
    /// The reason, in words.
    pub reason: String,
    /// Action-specific detail.
    pub detail: ValueMap,
    /// How long it may take.
    pub timeout: Option<Duration>,
}
