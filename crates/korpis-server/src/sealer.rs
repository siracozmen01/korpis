// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sealing assignments with the control plane's assignment key (§4.8 of
//! docs/02-architecture.md): the encoded `korpis.agent.v1.Assignment` and an Ed25519 signature
//! over exactly those bytes, so an agent can prove what it was told and a restored control
//! plane can recover what it forgot (§8 of docs/03-state.md).

use buffa::Message as _;
use ed25519_dalek::{Signer as _, SigningKey};
use korpis_app::ports::{AssignmentSealer, Opened, SealError, Sealed, VerifyKey};
use korpis_core::id::{Id, IdKind};
use korpis_core::labels::Labels;
use korpis_core::workload::assignment::{Assignment, IdRange};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_wire::assignment::assignment_to_pb;
use korpis_wire::plan::plan_from_pb;
use korpis_wire::run::ref_from_pb;
use korpis_wire::workload::{intent_from_pb, power_from_pb};
use sha2::{Digest, Sha256};

/// The sealer.
pub struct Ed25519Sealer {
    key: SigningKey,
    key_id: String,
}

impl std::fmt::Debug for Ed25519Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ed25519Sealer").field("key_id", &self.key_id).finish_non_exhaustive()
    }
}

impl Ed25519Sealer {
    /// Seals with `key`, named `key_id`.
    #[must_use]
    pub fn new(key: SigningKey, key_id: String) -> Self {
        Self { key, key_id }
    }
}

impl AssignmentSealer for Ed25519Sealer {
    fn content_digest(&self, assignment: &Assignment) -> Result<Vec<u8>, SealError> {
        let mut pb = assignment_to_pb(assignment);
        pb.generation = 0;
        Ok(Sha256::digest(pb.encode_to_vec()).to_vec())
    }

    fn seal(&self, assignment: &Assignment) -> Result<Sealed, SealError> {
        let body = assignment_to_pb(assignment).encode_to_vec();
        let signature = self.key.sign(&body).to_bytes().to_vec();
        Ok(Sealed { body, signature, key_id: self.key_id.clone() })
    }

    fn open(&self, sealed: &Sealed, keys: &[VerifyKey]) -> Result<Opened, SealError> {
        let refused = |why: &str| SealError(format!("{why} ({})", sealed.key_id));
        let key = keys
            .iter()
            .find(|k| k.key_id == sealed.key_id)
            .ok_or_else(|| refused("no assignment key of that name"))?;
        if !korpis_core::node::identity::verify(&key.public, &sealed.body, &sealed.signature) {
            return Err(refused("the signature does not verify"));
        }
        let pb = agent::Assignment::decode_from_slice(&sealed.body)
            .map_err(|_| refused("not an assignment"))?;
        let id = |kind, field: &str, value: &str| {
            Id::parse(kind, value).map(|i| i.uuid()).map_err(|_| refused(field))
        };
        let target = pb.target.as_option().ok_or_else(|| refused("no target"))?;
        let power = pb.power.as_option().ok_or_else(|| refused("no power"))?;
        let labels = pb.workload_labels.iter().map(|(k, v)| (k.clone(), v.clone()));
        Ok(Opened {
            workload: id(IdKind::Workload, "its workload", &pb.workload)?,
            node: id(IdKind::Node, "its node", &pb.node)?,
            generation: pb.generation,
            lease_epoch: pb.lease_epoch,
            project: id(IdKind::Project, "its project", &pb.project)?,
            organization: id(IdKind::Organization, "its organization", &pb.organization)?,
            name: (!pb.workload_name.is_empty())
                .then(|| korpis_wire::name("workload_name", &pb.workload_name))
                .transpose()
                .map_err(|e| refused(&e.to_string()))?,
            labels: Labels::new(labels).map_err(|e| refused(&e.to_string()))?,
            target: intent_from_pb("target", target).map_err(|e| refused(&e.to_string()))?,
            power: power_from_pb("power", power).map_err(|e| refused(&e.to_string()))?,
            residual: pb.residual,
            ids: IdRange { base: pb.uid_base, count: pb.uid_count },
            plan: pb
                .plan
                .as_option()
                .and_then(|p| p.whole.as_option())
                .map(plan_from_pb)
                .transpose()
                .map_err(|e| refused(&e.to_string()))?,
            runs: pb
                .runs
                .iter()
                .map(ref_from_pb)
                .collect::<Result<_, _>>()
                .map_err(|e| refused(&e.to_string()))?,
        })
    }
}
