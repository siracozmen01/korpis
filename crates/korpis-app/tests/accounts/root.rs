// SPDX-License-Identifier: AGPL-3.0-or-later

//! The root account (§9.1 and §9.2 of docs/08-identity.md): installation creates it holding the
//! root grant, invited for an hour by an invitation only a passkey redeems; and recovery at the
//! machine creates another, recorded with who ran it.

use crate::support::{Bound, ORIGIN, RP, browser, signed_in};
use korpis_app::account::{NewRegistration, PasswordRedemption};
use korpis_app::install::{RootCredential, RootHolder};
use korpis_app::ports::{EffectFilter, EffectRead as _, SystemClock};
use korpis_app::{App, AppError, NoRecipes};
use korpis_core::account::totp::TotpSecret;
use korpis_core::authz::token::SigningKey;
use korpis_core::tenancy::Name;
use korpis_core::webauthn::RelyingParty;
use korpis_store::testing::TestDb;
use std::sync::Arc;
use zeroize::Zeroizing;

async fn app(db: &TestDb) -> App<korpis_store::PgStore, SystemClock, NoRecipes> {
    App::new(db.store().await.unwrap(), SystemClock, Arc::new(SigningKey::generate(1)), NoRecipes)
        .with_relying_party(RelyingParty { id: RP.into(), origins: vec![ORIGIN.into()] })
        .with_factor_vault(Arc::new(Bound))
}

#[tokio::test]
async fn the_installed_root_account_is_redeemed_by_a_passkey_alone() {
    let db = TestDb::new().await.unwrap();
    let app = app(&db).await;
    let installed = app.install(Name::parse("acme").unwrap(), RootHolder::Passkey).await.unwrap();
    assert!(installed.token().is_none());
    let RootCredential::Invitation(root) = &installed.root else { panic!("a token holds root") };
    assert_eq!(installed.root_grant, root.grant);
    let invitation = root.secret.expose();
    assert!(app.invitation(invitation).await.unwrap().passkey_only);

    let totp = TotpSecret::generate().unwrap();
    let with_password = PasswordRedemption {
        name: "admin".into(),
        password: Zeroizing::new("correct horse battery".into()),
        code: crate::support::code(&totp, 0),
        totp,
    };
    let refused = app.redeem_with_password(invitation, with_password).await.unwrap_err();
    assert!(matches!(refused, AppError::Invalid { ref field, .. } if field == "invitation"));

    let options = app.begin_redemption(invitation, "admin").await.unwrap();
    let created = browser().create(&options.challenge);
    let r = NewRegistration {
        challenge: options.challenge_id,
        label: "laptop".into(),
        client_data_json: created.client_data_json,
        attestation_object: created.attestation_object,
    };
    let redeemed = app.redeem_with_passkey(invitation, "admin", r).await.unwrap();
    assert_eq!(redeemed.account, root.account);
    let held = app.own_account(&signed_in(root.account)).await.unwrap();
    assert_eq!(held.factors.passkeys, 1);
}

#[tokio::test]
async fn recovering_root_at_the_machine_invites_a_new_account_and_says_who_asked() {
    let db = TestDb::new().await.unwrap();
    let app = app(&db).await;
    let refused = app.recover_root("uid 0 (root)").await.unwrap_err();
    assert!(matches!(refused, AppError::Conflict { reason: "install.missing", .. }));
    let installed = app.install(Name::parse("acme").unwrap(), RootHolder::Token).await.unwrap();

    let recovered = app.recover_root("uid 0 (root), by sudo from alice").await.unwrap();
    assert_ne!(recovered.grant, installed.root_grant);
    assert!(app.invitation(recovered.secret.expose()).await.unwrap().passkey_only);
    let all = EffectFilter { workload: None, plan: None, node: None, since: None };
    let effects = app.store().effects(&all, None, 100).await.unwrap();
    let said = effects.iter().find(|e| e.action == "root.recovered").unwrap();
    let operator = said.after.as_ref().and_then(|a| a.get("operator")).and_then(|o| o.as_str());
    assert_eq!(operator, Some("uid 0 (root), by sudo from alice"));
    assert_eq!(said.grant, Some(recovered.grant));
}
