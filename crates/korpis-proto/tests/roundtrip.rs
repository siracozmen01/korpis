// SPDX-License-Identifier: Apache-2.0

//! The generated schema round-trips through both encodings the API serves, and the JSON names
//! are the canonical `ProtoJSON` ones a `curl` user writes (§3 of docs/10-api.md).

use buffa::Message;
use korpis_proto::korpis::agent::v1::{Assignment, NodePlan, SignedAssignment};
use korpis_proto::korpis::v1::{
    Intent, IntentBody, OnLeaseExpiry, Power, PowerState, RecipeRef, Step, StepExecutor,
};

fn sample_assignment() -> Assignment {
    Assignment {
        workload: "wl_7f3a".into(),
        project: "prj_1".into(),
        organization: "org_1".into(),
        generation: 3,
        target: Intent {
            id: "int_9".into(),
            workload: "wl_7f3a".into(),
            version: 2,
            body: IntentBody {
                recipe: RecipeRef {
                    name: "korpis.io/minecraft/paper".into(),
                    version: "1.21.4".into(),
                    digest: "sha256:00".into(),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        }
        .into(),
        power: Power {
            workload: "wl_7f3a".into(),
            desired: PowerState::POWER_STATE_RUNNING.into(),
            cycle: 1,
            version: 4,
            ..Default::default()
        }
        .into(),
        on_lease_expiry: OnLeaseExpiry::ON_LEASE_EXPIRY_KEEP_RUNNING.into(),
        plan: NodePlan {
            plan: "pln_1".into(),
            steps: vec![Step {
                index: 0,
                action: "create".into(),
                executor: StepExecutor::STEP_EXECUTOR_NODE.into(),
                ..Default::default()
            }],
            through: 0,
            ..Default::default()
        }
        .into(),
        lease_epoch: 7,
        ..Default::default()
    }
}

#[test]
fn assignment_round_trips_in_binary() {
    let original = sample_assignment();
    let bytes = original.encode_to_vec();
    let decoded = Assignment::decode_from_slice(&bytes).expect("decode");
    assert_eq!(decoded, original);

    // The signed form carries the exact bytes, which is what the signature covers.
    let signed = SignedAssignment { assignment: bytes.clone(), ..Default::default() };
    let back = SignedAssignment::decode_from_slice(&signed.encode_to_vec()).expect("decode");
    assert_eq!(back.assignment, bytes);
}

#[test]
fn assignment_round_trips_in_json_with_canonical_names() {
    let original = sample_assignment();
    let json = serde_json::to_string(&original).expect("encode json");
    assert!(json.contains("\"leaseEpoch\":\"7\""), "int64 as string, camelCase: {json}");
    assert!(json.contains("\"POWER_STATE_RUNNING\""), "enums by name: {json}");
    let decoded: Assignment = serde_json::from_str(&json).expect("decode json");
    assert_eq!(decoded, original);
}
