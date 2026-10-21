// SPDX-License-Identifier: AGPL-3.0-or-later

//! Accounts, invitations, factors, and challenges (§9.6 of docs/08-identity.md): every
//! single-use thing is spent once, however the requests race.

use crate::support::{fixture, now};
use korpis_app::ports::{AccountRead, AccountWrite, SealedKey, Store, StoreError, Tx, Unique};
use korpis_core::account::record::{
    Account, CHALLENGE_LIFETIME, Ceremony, Challenge, Factors, Invitation, Passkey,
};
use korpis_core::webauthn::Algorithm;
use std::time::Duration;
use uuid::Uuid;

fn account(name: Option<&str>) -> Account {
    Account {
        id: Uuid::now_v7(),
        display_name: "A Friend".into(),
        name: name.map(Into::into),
        created_by: "token:00000000-0000-0000-0000-000000000001".into(),
        created_at: now(),
        disabled_at: None,
    }
}

fn passkey(account: Uuid, id: u8) -> Passkey {
    Passkey {
        credential_id: vec![id; 16],
        account,
        public_key: vec![1, 2, 3],
        algorithm: Algorithm::Es256,
        sign_count: 0,
        aaguid: [0; 16],
        backup_eligible: true,
        label: "laptop".into(),
        created_at: now(),
        last_used_at: None,
        portal: None,
    }
}

#[tokio::test]
async fn an_invitation_is_redeemed_once_and_not_after_it_expires() {
    let f = fixture().await;
    let a = account(None);
    let invitation = Invitation {
        id: Uuid::now_v7(),
        account: a.id,
        issued_by: a.created_by.clone(),
        expires_at: now() + Duration::from_secs(60),
        passkey_only: false,
        redeemed_at: None,
    };
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_account(&a).await.unwrap();
    tx.insert_invitation(&invitation, &[7; 32]).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.store.account(a.id).await.unwrap(), Some(a.clone()));
    assert_eq!(f.store.invitation_by_digest(&[7; 32]).await.unwrap(), Some(invitation.clone()));
    assert_eq!(f.store.invitation_by_digest(&[8; 32]).await.unwrap(), None);

    let mut tx = f.store.begin().await.unwrap();
    let late = invitation.expires_at + Duration::from_secs(1);
    assert!(!tx.redeem_invitation(invitation.id, late).await.unwrap(), "expired");
    assert!(tx.redeem_invitation(invitation.id, now()).await.unwrap());
    assert!(!tx.redeem_invitation(invitation.id, now()).await.unwrap(), "spent");
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn a_sign_in_name_and_a_credential_each_belong_to_one_account() {
    let f = fixture().await;
    let (a, b) = (account(Some("friend")), account(None));
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_account(&a).await.unwrap();
    tx.insert_account(&b).await.unwrap();
    tx.insert_passkey(&passkey(a.id, 1)).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.store.account_named("friend").await.unwrap().map(|x| x.id), Some(a.id));

    let mut tx = f.store.begin().await.unwrap();
    let taken = tx.name_account(b.id, "friend").await;
    assert_eq!(taken, Err(StoreError::Duplicate(Unique::AccountName)));
    drop(tx);
    let mut tx = f.store.begin().await.unwrap();
    let taken = tx.insert_passkey(&passkey(b.id, 1)).await;
    assert_eq!(taken, Err(StoreError::Duplicate(Unique::Passkey)));
    drop(tx);
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.name_account(b.id, "UPPER").await.is_err(), "the store holds names lowercase");
}

#[tokio::test]
async fn factors_are_counted_and_each_code_or_counter_is_spent_once() {
    let f = fixture().await;
    let a = account(None);
    let sealed = SealedKey { sealed_by: "factor-1".into(), sealed: vec![9; 40] };
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_account(&a).await.unwrap();
    tx.insert_passkey(&passkey(a.id, 2)).await.unwrap();
    tx.set_password(a.id, "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA", now()).await.unwrap();
    tx.set_totp(a.id, &sealed, 100).await.unwrap();
    tx.replace_recovery_codes(a.id, &[[1; 32], [2; 32]]).await.unwrap();
    tx.commit().await.unwrap();
    let factors = Factors { passkeys: 1, password: true, totp: true, recovery_codes: 2 };
    assert_eq!(f.store.factors(a.id).await.unwrap(), factors);
    assert_eq!(f.store.totp(a.id).await.unwrap().unwrap().last_step, Some(100));

    let mut tx = f.store.begin().await.unwrap();
    assert!(!tx.totp_used(a.id, 100).await.unwrap(), "the confirming code's step is spent");
    assert!(tx.totp_used(a.id, 101).await.unwrap());
    assert!(tx.use_recovery_code(a.id, &[1; 32], now()).await.unwrap());
    assert!(!tx.use_recovery_code(a.id, &[1; 32], now()).await.unwrap(), "spent");
    assert!(tx.passkey_used(&[2; 16], 0, 5, now()).await.unwrap());
    assert!(!tx.passkey_used(&[2; 16], 0, 6, now()).await.unwrap(), "the counter moved");
    tx.commit().await.unwrap();
    assert_eq!(f.store.factors(a.id).await.unwrap().recovery_codes, 1);
    assert_eq!(f.store.passkey(&[2; 16]).await.unwrap().unwrap().sign_count, 5);

    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.remove_passkey(a.id, &[2; 16]).await.unwrap());
    assert!(tx.remove_password(a.id).await.unwrap());
    assert!(tx.remove_totp(a.id).await.unwrap());
    assert!(!tx.remove_totp(a.id).await.unwrap());
    tx.replace_recovery_codes(a.id, &[]).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.store.factors(a.id).await.unwrap(), Factors::default());
}

#[tokio::test]
async fn a_challenge_answers_one_ceremony_before_it_expires() {
    let f = fixture().await;
    let challenge = |expires_at| Challenge {
        id: Uuid::now_v7(),
        ceremony: Ceremony::SignIn,
        challenge: [3; 32],
        account: None,
        invitation: None,
        portal: None,
        expires_at,
    };
    let (live, stale) = (challenge(now() + CHALLENGE_LIFETIME), challenge(now()));
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_challenge(&stale, now() - Duration::from_secs(1)).await.unwrap();
    tx.insert_challenge(&live, now()).await.unwrap();
    assert_eq!(tx.take_challenge(live.id, now()).await.unwrap(), Some(live.clone()));
    assert_eq!(tx.take_challenge(live.id, now()).await.unwrap(), None, "taken");
    let later = now() + Duration::from_secs(1);
    assert_eq!(tx.take_challenge(stale.id, later).await.unwrap(), None, "expired");
    tx.commit().await.unwrap();
}
