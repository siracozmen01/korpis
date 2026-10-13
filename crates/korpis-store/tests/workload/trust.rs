// SPDX-License-Identifier: AGPL-3.0-or-later

//! Trust policies: the nearest one set governs, and a removal keeps the version counting.

use crate::support::{alice, fixture, now};
use korpis_app::ports::{PolicyChange, Store, TrustRead, TrustWrite, Tx};
use korpis_core::recipe::trust::{Signer, TrustPolicy};
use uuid::Uuid;

fn change(organization: Uuid, policy: Option<TrustPolicy>, version: u64) -> PolicyChange {
    PolicyChange { organization, policy, version, set_by: alice(), set_at: now() }
}

fn accepting(name: &str) -> TrustPolicy {
    TrustPolicy {
        accepted_signers: vec![Signer {
            name: name.to_owned(),
            public_key_pem: "-----BEGIN PUBLIC KEY-----".to_owned(),
            fingerprint: format!("sha256:{name}"),
        }],
        allow_unsigned: false,
        allow_unverified: true,
    }
}

#[tokio::test]
async fn the_nearest_policy_governs_and_a_removal_keeps_the_version_counting() {
    let f = fixture().await;
    let child = Uuid::now_v7();
    sqlx::query("INSERT INTO organization (id, name, parent_id) VALUES ($1, 'customer', $2)")
        .bind(child)
        .bind(f.organization)
        .execute(f.store.pool())
        .await
        .unwrap();
    assert_eq!(f.store.trust_policy(child).await.unwrap(), None, "the default governs");
    let apply = |c: PolicyChange, expected: u64| {
        let store = f.store.clone();
        async move {
            let mut tx = store.begin().await.unwrap();
            let done = tx.set_trust_policy(&c, expected).await.unwrap();
            tx.commit().await.unwrap();
            done
        }
    };
    assert!(apply(change(f.organization, Some(accepting("root")), 1), 0).await);
    let set = f.store.trust_policy(child).await.unwrap().unwrap();
    assert_eq!((set.organization, set.policy), (f.organization, accepting("root")));
    assert!(apply(change(child, Some(accepting("own")), 1), 0).await);
    assert!(!apply(change(child, Some(accepting("again")), 1), 0).await, "already set");
    let set = f.store.trust_policy(child).await.unwrap().unwrap();
    assert_eq!((set.organization, set.version), (child, 1));

    assert!(apply(change(child, None, 2), 1).await);
    let set = f.store.trust_policy(child).await.unwrap().unwrap();
    assert_eq!(set.organization, f.organization, "the parent's governs again");
    assert_eq!(f.store.own_trust_version(child).await.unwrap(), 2);
    assert!(!apply(change(child, Some(accepting("stale")), 2), 1).await, "no going back to 1");
    assert!(apply(change(child, Some(accepting("back")), 3), 2).await);
    let set = f.store.trust_policy(child).await.unwrap().unwrap();
    assert_eq!((set.organization, set.version), (child, 3));
}
