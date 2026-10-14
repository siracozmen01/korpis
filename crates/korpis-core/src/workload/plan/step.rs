// SPDX-License-Identifier: AGPL-3.0-or-later

//! Steps, the action vocabulary, and where `disruptive` comes from (§3.3 of docs/01-model.md,
//! finding R9): one override chain, every layer visible, and no layer below the operator able to
//! make a step less disruptive than the layer beneath it.

use crate::node::driver::DriverInfo;
use crate::object::ObjectRef;
use crate::recipe::Disruption;
use crate::value::ValueMap;
use crate::workload::spec::IsolationTier;
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
    /// Take a snapshot of a volume.
    SnapshotVolume,
    /// Make a volume what one of its snapshots holds; what was written since is lost.
    RollbackVolume,
    /// Delete one snapshot of a volume.
    DeleteSnapshot,
    /// Back a volume up into a repository.
    BackupVolume,
    /// Restore a backup into a volume: into a new directory of it, or into a staging place
    /// beside it that a later step makes the volume.
    RestoreVolume,
    /// Make a volume what its staged restore holds; what it held is lost.
    ReplaceVolume,
    /// Forget the backups a retention policy no longer keeps, and mark what only they held for
    /// deletion (§5.3 of docs/06-storage.md).
    PruneBackups,
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
            Self::SnapshotVolume => "snapshot_volume",
            Self::RollbackVolume => "rollback_volume",
            Self::DeleteSnapshot => "delete_snapshot",
            Self::BackupVolume => "backup_volume",
            Self::RestoreVolume => "restore_volume",
            Self::ReplaceVolume => "replace_volume",
            Self::PruneBackups => "prune_backups",
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

    const ALL: [Self; 21] = [
        Self::PullRecipe,
        Self::CreateVolume,
        Self::AttachVolume,
        Self::DetachVolume,
        Self::DeleteVolume,
        Self::SnapshotVolume,
        Self::RollbackVolume,
        Self::DeleteSnapshot,
        Self::BackupVolume,
        Self::RestoreVolume,
        Self::ReplaceVolume,
        Self::PruneBackups,
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
            Self::DeleteVolume
            | Self::ReleaseAddress
            | Self::RollbackVolume
            | Self::ReplaceVolume => (true, false),
            Self::DeleteSnapshot | Self::PruneBackups => (false, false),
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

impl DriverClass {
    /// The driver layer for a workload at `tier`, from every driver that could run it: the
    /// union of what each makes disruptive there. A union, because the layer may only make a
    /// step more disruptive, so the strictest declaration is the one no placement can undercut,
    /// and a plan computed before a workload is placed is classified as it will be after.
    #[must_use]
    pub fn of<'a>(drivers: impl IntoIterator<Item = &'a DriverInfo>, tier: IsolationTier) -> Self {
        let mut disruptive: Vec<StepAction> = drivers
            .into_iter()
            .filter(|d| d.capabilities.tiers.contains(&tier))
            .flat_map(|d| &d.capabilities.disruptive)
            .filter(|(t, _)| *t == tier)
            .flat_map(|(_, actions)| actions.iter().copied())
            .collect();
        disruptive.sort_by_key(|a| a.as_str());
        disruptive.dedup();
        Self { disruptive }
    }
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

/// Why a set of overrides cannot be recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideRefusal {
    /// The action is overridden twice; which entry wins would be a guess.
    Duplicate(StepAction),
    /// It would make an action the vocabulary holds disruptive less so, which no layer may.
    BelowFloor(StepAction),
}

impl Override {
    /// Checks a whole set of overrides for one workload: at most one per action, and none that
    /// relaxes the vocabulary's floor. The classifier would ignore such an entry; refusing it
    /// instead keeps the record saying only what takes effect.
    ///
    /// # Errors
    /// The first entry that cannot be recorded, and why.
    pub fn check(set: &[Self]) -> Result<(), OverrideRefusal> {
        let mut seen = Vec::with_capacity(set.len());
        for o in set {
            if seen.contains(&o.action) {
                return Err(OverrideRefusal::Duplicate(o.action));
            }
            seen.push(o.action);
            if o.action.floor().0 && !o.disruptive {
                return Err(OverrideRefusal::BelowFloor(o.action));
            }
        }
        Ok(())
    }
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
