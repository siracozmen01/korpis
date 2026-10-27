// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::labels::Labels;
use crate::principal::SystemActor;
use crate::recipe::RecipeRef;
use crate::value::ValueMap;
use crate::workload::spec::{
    ConsoleMode, Continuity, HealthSpec, IntentBody, IsolationTier, Lifecycle, PlacementSpec,
    ResourceSpec, RuntimeSpec,
};

fn body() -> IntentBody {
    IntentBody {
        recipe: RecipeRef {
            name: "r".into(),
            version: "1".into(),
            digest: String::new(),
            trust: None,
            providers: Vec::new(),
        },
        lifecycle: Lifecycle::Persistent,
        schedule: None,
        timezone: None,
        runs: None,
        console: ConsoleMode::None,
        runtime: RuntimeSpec {
            driver: "oci".into(),
            tier: IsolationTier::Container,
            config: ValueMap::new(),
        },
        resources: ResourceSpec::default(),
        devices: Vec::new(),
        volumes: Vec::new(),
        endpoints: Vec::new(),
        health: HealthSpec::default(),
        config: ValueMap::new(),
        placement: PlacementSpec::default(),
        continuity: Continuity::Derived,
        replica_failover: crate::workload::spec::ReplicaFailover::Manual,
        dependencies: Vec::new(),
        streams: crate::workload::spec::StreamSpec::default(),
        egress: crate::workload::spec::EgressPolicy::default(),
    }
}

fn set(mode: DriftMode) -> DeclaredSet {
    DeclaredSet {
        id: Uuid::from_u128(1),
        project: Uuid::from_u128(2),
        name: Name::parse("servers").unwrap_or_else(|_| unreachable!()),
        mode,
        version: 1,
        created_by: Principal::System(SystemActor::Bootstrap),
        created_at: SystemTime::UNIX_EPOCH,
    }
}

fn workload(in_set: Option<u128>) -> Workload {
    Workload {
        id: Uuid::from_u128(3),
        project: Uuid::from_u128(2),
        organization: Uuid::from_u128(4),
        name: Name::parse("mc").unwrap_or_else(|_| unreachable!()),
        labels: Labels::default(),
        intent: Uuid::from_u128(5),
        intent_version: 1,
        target: None,
        target_version: 0,
        power_version: 1,
        power_target_version: 0,
        version: 1,
        created: SystemTime::UNIX_EPOCH,
        deleted_at: None,
        set: in_set.map(Uuid::from_u128),
        classification: Vec::new(),
        unreproducible: None,
    }
}

fn head(managed_by: Option<u128>) -> Intent {
    Intent {
        id: Uuid::from_u128(5),
        workload: Uuid::from_u128(3),
        version: 1,
        parent: None,
        declared_by: Principal::System(SystemActor::Bootstrap),
        declared_at: SystemTime::UNIX_EPOCH,
        body: body(),
        managed_by: managed_by.map(Uuid::from_u128),
    }
}

#[test]
fn a_member_declared_outside_its_set_has_drifted() {
    assert!(!drifted(&workload(None), &head(None)), "no set, no drift");
    assert!(!drifted(&workload(Some(1)), &head(Some(1))));
    assert!(drifted(&workload(Some(1)), &head(None)), "the panel declared it");
    assert!(drifted(&workload(Some(1)), &head(Some(9))));
}

#[test]
fn a_set_touches_only_its_own_and_never_loses_a_fix_silently() {
    let (advisory, strict) = (set(DriftMode::Advisory), set(DriftMode::Strict));
    let member = workload(Some(1));
    let (ours, theirs) = (head(Some(1)), head(None));
    assert_eq!(through(&advisory, &member, &ours, false, false, false), Ok(Through::Member));
    let other = workload(Some(9));
    assert_eq!(
        through(&strict, &other, &head(Some(9)), false, true, true),
        Err(SetRefusal::OtherSet)
    );
    let loose = workload(None);
    assert_eq!(
        through(&advisory, &loose, &theirs, false, false, false),
        Err(SetRefusal::NotAMember)
    );
    assert_eq!(through(&advisory, &loose, &theirs, false, true, false), Ok(Through::Adopted));

    assert_eq!(through(&advisory, &member, &theirs, true, false, false), Ok(Through::DriftAdopted));
    assert_eq!(through(&advisory, &member, &theirs, false, false, false), Err(SetRefusal::Drifted));
    let reconciled = through(&advisory, &member, &theirs, false, false, true);
    assert_eq!(reconciled, Ok(Through::DriftDiscarded));
    assert_eq!(
        through(&strict, &member, &theirs, false, false, false),
        Ok(Through::DriftDiscarded)
    );
}

#[test]
fn a_mode_reads_back_as_stored() {
    for m in [DriftMode::Advisory, DriftMode::Strict] {
        assert_eq!(DriftMode::parse(m.as_str()), Some(m));
    }
    assert_eq!(DriftMode::parse("lenient"), None);
}
