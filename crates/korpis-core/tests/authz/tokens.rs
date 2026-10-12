// SPDX-License-Identifier: AGPL-3.0-or-later

//! Capability tokens, verified offline (§6 of docs/08-identity.md).

use korpis_core::authz::token::{self, Claims, KeyRing, SigningKey, TokenError};
use korpis_core::authz::{Hierarchy, Resource};
use std::collections::HashSet;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

use crate::support::Tree;

const CONSOLE: &str = "workload.console.write";

/// A key, a ring trusting it, and a token for the console of W1 through a three-grant chain.
struct Fixture {
    tree: Tree,
    now: SystemTime,
    key: SigningKey,
    keys: KeyRing,
    claims: Claims,
    token: String,
    w1: Resource,
}

impl Fixture {
    fn new() -> Self {
        let tree = Tree::new();
        let now = SystemTime::now();
        let key = SigningKey::generate(1);
        let mut keys = KeyRing::default();
        keys.insert(1, &key.public_bytes()).unwrap();
        let claims = Claims {
            subject: "user:alice".into(),
            chain: vec![Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()],
            actions: vec![CONSOLE.into()],
            scope: tree.w1,
            expires_at: now + token::SESSION_TOKEN_LIFETIME,
        };
        let token = token::mint(&key, &claims).unwrap();
        let w1 = tree.resource(tree.w1).unwrap();
        Self { tree, now, key, keys, claims, token, w1 }
    }

    fn verify(
        &self,
        t: &str,
        action: &str,
        on: &Resource,
        at: SystemTime,
    ) -> Result<(), TokenError> {
        token::verify(t, &self.keys, action, on, &HashSet::new(), at).map(|_| ())
    }
}

#[test]
fn a_token_permits_exactly_what_it_states() {
    let f = Fixture::new();
    let ok = token::verify(&f.token, &f.keys, CONSOLE, &f.w1, &HashSet::new(), f.now).unwrap();
    assert_eq!(ok.grant, f.claims.chain[0]);
    assert_eq!((ok.chain.len(), ok.subject.as_str()), (3, "user:alice"));

    assert!(f.verify(&f.token, "workload.exec", &f.w1, f.now).is_err());
    let w2 = f.tree.resource(f.tree.w2).unwrap();
    assert!(f.verify(&f.token, CONSOLE, &w2, f.now).is_err());
    let late = f.now + Duration::from_secs(121);
    assert!(f.verify(&f.token, CONSOLE, &f.w1, late).is_err());

    let parent = f.claims.chain[1];
    assert_eq!(
        token::verify(&f.token, &f.keys, CONSOLE, &f.w1, &HashSet::from([parent]), f.now),
        Err(TokenError::Revoked(parent)),
        "revoking any link of the chain refuses the token offline (§6.3)"
    );
}

#[test]
fn expiry_only_shortens_keys_restore_and_retired_keys_verify_nothing() {
    let mut f = Fixture::new();
    let shorter =
        token::restrict_expiry(&f.token, &f.keys, f.now + Duration::from_secs(10)).unwrap();
    f.verify(&shorter, CONSOLE, &f.w1, f.now).unwrap();
    assert!(f.verify(&shorter, CONSOLE, &f.w1, f.now + Duration::from_secs(11)).is_err());

    let restored = SigningKey::from_bytes(1, &f.key.private_bytes()).unwrap();
    let again = token::mint(&restored, &f.claims).unwrap();
    f.verify(&again, CONSOLE, &f.w1, f.now).unwrap();

    f.keys.retire(1);
    assert!(f.verify(&f.token, CONSOLE, &f.w1, f.now).is_err());
}

#[test]
fn tokens_scoped_above_a_customer_reach_no_private_action_offline() {
    let f = Fixture::new();
    let mint = |scope, action: &str| {
        let claims = Claims {
            subject: "user:host-admin".into(),
            chain: vec![Uuid::now_v7()],
            actions: vec![action.into()],
            scope,
            expires_at: f.now + Duration::from_secs(60),
        };
        token::mint(&f.key, &claims).unwrap()
    };
    let above = mint(f.tree.reseller, CONSOLE);
    assert!(f.verify(&above, CONSOLE, &f.w1, f.now).is_err());
    let within = mint(f.tree.customer, CONSOLE);
    f.verify(&within, CONSOLE, &f.w1, f.now).unwrap();
    let operate = mint(f.tree.reseller, "workload.stop");
    f.verify(&operate, "workload.stop", &f.w1, f.now).expect("non-private actions still flow down");
}
