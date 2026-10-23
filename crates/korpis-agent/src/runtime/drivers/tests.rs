// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::runtime::memory::InMemory;

/// An intent for the driver named `driver`.
fn intent(id: &str, driver: &str) -> pb::Intent {
    let runtime = pb::RuntimeSpec { driver: driver.into(), ..Default::default() };
    let body = pb::IntentBody { runtime: runtime.into(), ..Default::default() };
    pb::Intent { id: id.into(), body: body.into(), ..Default::default() }
}

async fn made(on: &Drivers<InMemory>, workload: &str, driver: &str) -> String {
    let Prepared::Spec(spec) = on.prepare(workload, &intent("in_1", driver), b"").await.unwrap()
    else {
        panic!("the memory runtime accepts everything unless told otherwise");
    };
    assert_eq!(spec.driver, driver, "the driver names itself in what it prepared");
    on.create(&spec).await.unwrap()
}

fn pair() -> (InMemory, InMemory) {
    (InMemory::named("oci", "oci"), InMemory::named("firecracker", "fc"))
}

#[tokio::test]
async fn each_verb_reaches_the_driver_that_made_the_object() {
    let (oci, fc) = pair();
    let drivers = Drivers::new(vec![oci.clone(), fc.clone()]).await.unwrap();
    let names: Vec<String> = drivers.drivers().await.unwrap().into_iter().map(|d| d.name).collect();
    assert_eq!(names, ["oci", "firecracker"], "each driver is reported as it declares itself");

    let vm = made(&drivers, "wl_vm", "firecracker").await;
    let box_ = made(&drivers, "wl_box", "oci").await;
    drivers.start(&vm).await.unwrap();
    drivers.start(&box_).await.unwrap();
    assert!(
        fc.calls().contains(&format!("start {vm}"))
            && !oci.calls().iter().any(|c| *c == format!("start {vm}"))
    );
    assert!(oci.calls().contains(&format!("start {box_}")));
    drivers.destroy(&vm).await.unwrap();
    assert!(fc.objects().is_empty() && oci.objects().len() == 1, "only its maker destroyed it");

    let refused = drivers.prepare("wl_x", &intent("in_2", "qemu"), b"").await.unwrap();
    let Prepared::Rejected(why) = refused else { panic!("a driver the node lacks is a rejection") };
    assert_eq!(
        why.reason.as_known(),
        Some(pb::UnsatisfiableReason::UNSATISFIABLE_REASON_MISSING_DRIVER)
    );
}

#[tokio::test]
async fn after_a_restart_objects_are_found_by_recovery_or_by_asking() {
    let (oci, fc) = pair();
    let before = Drivers::new(vec![oci.clone(), fc.clone()]).await.unwrap();
    let vm = made(&before, "wl_vm", "firecracker").await;
    let box_ = made(&before, "wl_box", "oci").await;

    let recovered = Drivers::new(vec![oci.clone(), fc.clone()]).await.unwrap();
    let ids: Vec<String> =
        recovered.recover().await.unwrap().into_iter().map(|o| o.runtime_id).collect();
    assert_eq!(ids, [box_.clone(), vm.clone()], "every driver's objects, identifiers unchanged");
    let asked = fc.calls().iter().filter(|c| c.starts_with("inspect")).count();
    recovered.start(&vm).await.unwrap();
    let after = fc.calls().iter().filter(|c| c.starts_with("inspect")).count();
    assert_eq!(asked, after, "a recovered object is routed by the table, without asking");

    let unaware = Drivers::new(vec![oci.clone(), fc.clone()]).await.unwrap();
    unaware.kill(&vm).await.unwrap();
    assert!(fc.calls().contains(&format!("kill {vm}")), "an object never seen is asked for");
    assert!(matches!(unaware.start("nothing-1").await, Err(RuntimeError::NotFound(_))));
}

#[tokio::test]
async fn every_driver_s_changes_arrive_and_one_name_twice_is_refused() {
    let (oci, fc) = pair();
    let drivers = Drivers::new(vec![oci.clone(), fc.clone()]).await.unwrap();
    let mut changes = drivers.changes();
    let vm = made(&drivers, "wl_vm", "firecracker").await;
    drivers.start(&vm).await.unwrap();
    fc.exit(&vm, 0);
    let seen = tokio::time::timeout(Duration::from_secs(5), changes.recv()).await.unwrap();
    assert_eq!(seen.unwrap(), vm);

    let twice = Drivers::new(vec![oci.clone(), InMemory::named("oci", "x")]).await;
    assert!(matches!(twice, Err(RuntimeError::Failed(_))));
    assert!(Drivers::<InMemory>::new(Vec::new()).await.is_err(), "a node with no driver");
}
