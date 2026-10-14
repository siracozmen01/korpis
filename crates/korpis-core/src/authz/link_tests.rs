// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::authz::token::{
    Claims, Client, mint as mint_session, restrict_expiry, verify as session,
};
use crate::scope::{Kind, ObjectRef, Resource};
use std::collections::HashSet;

fn ring(key: &SigningKey) -> KeyRing {
    let mut ring = KeyRing::default();
    ring.insert(key.key_id, &key.public_bytes()).unwrap();
    ring
}

fn hours(n: u64) -> Duration {
    Duration::from_secs(n * 3600)
}

#[test]
fn a_link_names_its_grant_until_it_expires_and_only_under_its_key() {
    let (key, now, grant) = (SigningKey::generate(1), SystemTime::now(), Uuid::now_v7());
    let link = mint(&key, grant, now + hours(24)).unwrap();
    assert!(link.starts_with(LINK_PREFIX));
    let found = verify(&link, &ring(&key), now).unwrap();
    assert_eq!(found.grant, grant);
    assert!(verify(&link, &ring(&key), now + hours(25)).is_err(), "expired");
    assert!(verify(&link, &ring(&SigningKey::generate(1)), now).is_err(), "another key");
    assert!(verify(&link[LINK_PREFIX.len()..], &ring(&key), now).is_err(), "without its prefix");
}

#[test]
fn a_link_is_refused_by_every_offline_verifier_and_a_session_token_is_not_a_link() {
    let (key, now, grant) = (SigningKey::generate(1), SystemTime::now(), Uuid::now_v7());
    let link = mint(&key, grant, now + hours(24)).unwrap();
    let workload = ObjectRef::new(Kind::Workload, Uuid::now_v7());
    let resource = Resource::alone(workload);
    let none: HashSet<Uuid> = HashSet::new();
    for action in ["workload.console.read", "workload.console.write", "workload.restart"] {
        let at = (now, Client::At("192.0.2.1".parse().unwrap()));
        let bare = &link[LINK_PREFIX.len()..];
        assert!(session(bare, &ring(&key), action, &resource, &none, at).is_err(), "{action}");
    }
    let claims = Claims {
        subject: format!("link:{grant}"),
        chain: vec![grant],
        actions: vec!["workload.restart".into()],
        scope: workload,
        expires_at: now + Duration::from_secs(120),
        source_cidr: Vec::new(),
    };
    let token = mint_session(&key, &claims).unwrap();
    assert!(verify(&format!("{LINK_PREFIX}{token}"), &ring(&key), now).is_err());
}

#[test]
fn its_holder_can_shorten_a_link_and_cannot_repoint_it() {
    let (key, now, grant) = (SigningKey::generate(1), SystemTime::now(), Uuid::now_v7());
    let link = mint(&key, grant, now + hours(24)).unwrap();
    let bare = &link[LINK_PREFIX.len()..];
    let shorter = restrict_expiry(bare, &ring(&key), now + hours(1)).unwrap();
    let shorter = format!("{LINK_PREFIX}{shorter}");
    let at = verify(&shorter, &ring(&key), now).unwrap().expires_at;
    let secs = |t: SystemTime| t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    assert_eq!(secs(at), secs(now + hours(1)), "a Biscuit date is in whole seconds");
    assert!(verify(&shorter, &ring(&key), now + hours(2)).is_err());

    let biscuit = ring(&key).parse(bare).unwrap();
    let other = Uuid::now_v7().to_string();
    let block = biscuit_auth::BlockBuilder::new().code(format!("link(\"{other}\");")).unwrap();
    let repointed = format!("{LINK_PREFIX}{}", biscuit.append(block).unwrap().to_base64().unwrap());
    let found = verify(&repointed, &ring(&key), now).unwrap();
    assert_eq!(found.grant, grant, "a holder's block names nothing the gateway believes");
}
