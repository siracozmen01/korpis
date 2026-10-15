// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipe references, `korpis.v1.RecipeRef`, and trust policies, `korpis.v1.TrustPolicy`.

use korpis_core::id::{Id, IdKind};
use korpis_core::recipe::trust::{Signer, TrustPolicy, Vouched};
use korpis_core::recipe::{ProviderPin, RecipeRef};
use korpis_proto::korpis::v1 as pb;

fn trust_to_pb(v: &Vouched) -> pb::RecipeTrust {
    match v {
        Vouched::Signer(name) => pb::RecipeTrust { signer: name.clone(), ..Default::default() },
        Vouched::Unsigned => pb::RecipeTrust { unsigned: true, ..Default::default() },
    }
}

/// What vouched for a recipe; `None` for an unset or malformed record, which resolution
/// replaces and a stored body never holds.
fn trust_from_pb(t: Option<&pb::RecipeTrust>) -> Option<Vouched> {
    let t = t?;
    match (t.signer.is_empty(), t.unsigned) {
        (false, false) => Some(Vouched::Signer(t.signer.clone())),
        (true, true) => Some(Vouched::Unsigned),
        _ => None,
    }
}

/// The wire form of a recipe reference.
#[must_use]
pub fn ref_to_pb(r: &RecipeRef) -> pb::RecipeRef {
    pb::RecipeRef {
        name: r.name.clone(),
        version: r.version.clone(),
        digest: r.digest.clone(),
        trust: r.trust.as_ref().map(trust_to_pb).into(),
        providers: r
            .providers
            .iter()
            .map(|p| pb::ProviderPin {
                step: p.step.clone(),
                extension: crate::wire_id(IdKind::Extension, p.extension),
                digest: p.digest.clone(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// A recipe reference as sent. Its form is checked with the body it belongs to, and an empty
/// digest is how a client asks for `name:version` to be resolved (§8 of docs/09-recipes.md).
#[must_use]
pub fn ref_from_pb(r: &pb::RecipeRef) -> RecipeRef {
    RecipeRef {
        name: r.name.clone(),
        version: r.version.clone(),
        digest: r.digest.clone(),
        trust: trust_from_pb(r.trust.as_option()),
        // Pinned by the control plane at declaration; a client's are not read. Stored intents
        // come back through here, so a pin that does not parse is kept out rather than invented.
        providers: r
            .providers
            .iter()
            .filter_map(|p| {
                let extension = Id::parse(IdKind::Extension, &p.extension).ok()?.uuid();
                Some(ProviderPin { step: p.step.clone(), extension, digest: p.digest.clone() })
            })
            .collect(),
    }
}

/// The wire form of a trust policy, with each signer's fingerprint.
#[must_use]
pub fn policy_to_pb(p: &TrustPolicy) -> pb::TrustPolicy {
    pb::TrustPolicy {
        accepted_signers: p
            .accepted_signers
            .iter()
            .map(|s| pb::Signer {
                name: s.name.clone(),
                public_key_pem: s.public_key_pem.clone(),
                fingerprint: s.fingerprint.clone(),
                ..Default::default()
            })
            .collect(),
        allow_unsigned: p.allow_unsigned,
        allow_unverified: p.allow_unverified,
        ..Default::default()
    }
}

/// A trust policy as sent. A fingerprint is output only and ignored: the use case computes it
/// from the key, so a policy cannot claim a key it does not carry.
#[must_use]
pub fn policy_from_pb(p: &pb::TrustPolicy) -> TrustPolicy {
    TrustPolicy {
        accepted_signers: p
            .accepted_signers
            .iter()
            .map(|s| Signer {
                name: s.name.clone(),
                public_key_pem: s.public_key_pem.clone(),
                fingerprint: String::new(),
            })
            .collect(),
        allow_unsigned: p.allow_unsigned,
        allow_unverified: p.allow_unverified,
    }
}
