// SPDX-License-Identifier: AGPL-3.0-or-later

//! The steps of a remake plan (§2.4 of docs/05-scheduling.md): a workload made again on the node
//! it was placed on again, with what it holds, and a volume whose only copy was on the node that
//! is gone made from the freshest replica of it the new node keeps (§2.2 of docs/06-storage.md).

use super::{Builder, int, text};
use crate::object::{Kind, ObjectRef};
use crate::value::ValueMap;
use crate::workload::PowerState;
use crate::workload::plan::step::{Classifier, Step, StepAction};
use crate::workload::spec::{IntentBody, VolumeSpec};
use uuid::Uuid;

/// A volume a workload holds, as a remake plan makes it hold it where it lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldVolume {
    /// The name it mounts it under.
    pub name: String,
    /// Its record.
    pub record: Uuid,
    /// The replica it is made from on the new node, where its own node is gone; `None` where
    /// the new node reaches it where it is.
    pub promote: Option<Promotion>,
}

/// A replica a volume is made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    /// The last snapshot the replica received whole.
    pub snapshot: Uuid,
    /// The class it is kept in, the volume's.
    pub storage_class: String,
    /// The helper's identifier the volume is made under on the new node: never the one it had,
    /// which a copy left there by an earlier failover may still hold.
    pub into: String,
}

impl Builder<'_> {
    /// Makes the volume `v` declares, whose record is `record`, what `p` holds on this node,
    /// limited to the size it declares.
    fn promote(&mut self, v: &VolumeSpec, record: Uuid, p: &Promotion) {
        let detail = ValueMap::from([
            ("volume".to_owned(), text(&v.name)),
            ("size_bytes".to_owned(), int(v.size_bytes)),
            ("existing".to_owned(), text(&record.to_string())),
            ("storage_class".to_owned(), text(&p.storage_class)),
            ("snapshot".to_owned(), text(&p.snapshot.to_string())),
            ("into".to_owned(), text(&p.into)),
        ]);
        self.push(StepAction::PromoteReplica, detail, &[]);
    }
}

/// The steps of a remake plan for `workload`: its approved `body` made again on the node it was
/// placed on again, its recipe pulled there, each volume it holds attached where it is or made
/// from its replica there, never made anew or written by an install, and its runtime created,
/// then started if it is to be running. A volume in `held`, by name, is the one its record
/// names; one with no record was never reported made, so it is created as on its first node.
#[must_use]
pub fn remake_steps(
    workload: Uuid,
    (body, power): (&IntentBody, PowerState),
    held: &[HeldVolume],
    classifier: Classifier<'_>,
) -> Vec<Step> {
    let mut b =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    b.pull(body);
    for v in &body.volumes {
        let found = held.iter().find(|h| h.name == v.name);
        let record = v.existing.or_else(|| found.map(|h| h.record));
        match (record, found.and_then(|h| h.promote.as_ref())) {
            (Some(record), Some(p)) => b.promote(v, record, p),
            _ => b.add_volume(&VolumeSpec { existing: record, from_backup: None, ..v.clone() }),
        }
    }
    b.push(StepAction::Create, ValueMap::new(), &[]);
    if power == PowerState::Running {
        b.push(StepAction::Start, ValueMap::new(), &[]);
    }
    b.steps
}
