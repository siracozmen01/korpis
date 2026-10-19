// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

#[tokio::test]
async fn objects_are_created_once_per_intent_and_outlive_whoever_holds_a_clone() {
    let runtime = InMemory::new();
    let intent = pb::Intent { id: "in_1".into(), ..Default::default() };
    let Prepared::Spec(spec) = runtime.prepare("wl_1", &intent, b"").await.unwrap() else {
        panic!("the memory runtime accepts everything unless told otherwise");
    };
    let id = runtime.create(&spec).await.unwrap();
    assert_eq!(runtime.create(&spec).await.unwrap(), id, "idempotent by workload and intent");
    runtime.start(&id).await.unwrap();
    let other = runtime.clone();
    assert_eq!(other.recover().await.unwrap().len(), 1);
    other.exit(&id, 1);
    let status = runtime.inspect(&id).await.unwrap();
    assert_eq!(status.exit_code, Some(1));
    runtime.fail("start", true);
    assert!(runtime.start(&id).await.is_err());
    runtime.destroy(&id).await.unwrap();
    assert!(runtime.objects().is_empty());
}
