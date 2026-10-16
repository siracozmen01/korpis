// SPDX-License-Identifier: AGPL-3.0-or-later

//! An installation to invite accounts into, a vault for their TOTP secrets, and a browser's
//! software authenticator.

use korpis_app::account::{Client, NewRegistration, Redeemed};
use korpis_app::authz::{Holder, IssueGrant};
use korpis_app::install::RootHolder;
use korpis_app::ports::{FactorVault, SealedKey, SystemClock, VaultError};
use korpis_app::{App, Caller, Idempotent, NoRecipes, Reply};
use korpis_core::account::session::{SessionKind, SessionLimits};
use korpis_core::account::totp::{STEP_SECONDS, TotpSecret};
use korpis_core::authz::token::SigningKey;
use korpis_core::authz::{Action, Conditions, Delegation};
use korpis_core::object::{Kind, ObjectRef};
use korpis_core::principal::Principal;
use korpis_core::scope::Scope;
use korpis_core::tenancy::Name;
use korpis_core::webauthn::RelyingParty;
use korpis_core::webauthn::testing::SoftAuthenticator;
use korpis_store::PgStore;
use korpis_store::testing::TestDb;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const RP: &str = "example.com";
pub const ORIGIN: &str = "https://panel.example.com";

/// A vault that binds each secret to its account and hides nothing: the binding is what the
/// use cases rely on, the cipher is the server's and tested there.
#[derive(Debug)]
pub struct Bound;

impl FactorVault for Bound {
    fn seal(&self, account: Uuid, secret: &TotpSecret) -> Result<SealedKey, VaultError> {
        let mut sealed = account.as_bytes().to_vec();
        sealed.extend_from_slice(secret.bytes());
        Ok(SealedKey { sealed_by: "test-1".into(), sealed })
    }

    fn open(&self, account: Uuid, sealed: &SealedKey) -> Result<TotpSecret, VaultError> {
        let forged = || VaultError::Forged(account.to_string());
        let (bound, secret) = sealed.sealed.split_at_checked(16).ok_or_else(forged)?;
        if bound != account.as_bytes() {
            return Err(forged());
        }
        Ok(TotpSecret::from_bytes(secret.try_into().map_err(|_| forged())?))
    }
}

pub struct Fixture {
    _db: TestDb,
    pub app: App<PgStore, SystemClock, NoRecipes>,
    pub root: Caller,
    pub organization: Uuid,
}

pub async fn fixture() -> Fixture {
    fixture_with(SessionLimits::default()).await
}

/// [`fixture`], its sessions ending by `limits`.
pub async fn fixture_with(limits: SessionLimits) -> Fixture {
    let db = TestDb::new().await.unwrap();
    let store = db.store().await.unwrap();
    let app = App::new(store, SystemClock, Arc::new(SigningKey::generate(1)), NoRecipes)
        .with_relying_party(RelyingParty { id: RP.into(), origins: vec![ORIGIN.into()] })
        .with_factor_vault(Arc::new(Bound))
        .with_session_limits(limits);
    let installed = app.install(Name::parse("acme").unwrap(), RootHolder::Token).await.unwrap();
    let root = Caller::new(Principal::Token(installed.token().unwrap().0.id), None, false);
    Fixture { _db: db, app, root, organization: installed.organization.id }
}

/// Issues `workload.read` on the organization to a new user called `display_name`; answers its
/// invitation's secret.
pub async fn invite(f: &Fixture, display_name: &str) -> String {
    invite_until(f, display_name, None).await.unwrap()
}

pub async fn invite_until(
    f: &Fixture,
    display_name: &str,
    invitation_expires_at: Option<SystemTime>,
) -> Result<String, korpis_app::AppError> {
    let key = Uuid::now_v7().to_string();
    let encode = |_: &korpis_app::authz::Issued| Vec::new();
    let idem = Idempotent { key: &key, encode: &encode };
    let cmd = IssueGrant {
        parent: None,
        holder: Holder::NewUser { display_name: display_name.into(), invitation_expires_at },
        actions: [Action::parse("workload.read").unwrap()].into(),
        scope: Scope::Object(ObjectRef::new(Kind::Organization, f.organization)),
        conditions: Conditions::default(),
        delegation: Delegation::default(),
        label: String::new(),
    };
    let Reply::Fresh(issued) = f.app.issue_grant(&f.root, &idem, cmd).await? else {
        panic!("a fresh key replayed");
    };
    assert!(matches!(issued.record.holder, Principal::User(_)));
    Ok(issued.invitation.unwrap().expose().to_owned())
}

/// A browser on the panel's origin.
pub fn browser() -> SoftAuthenticator {
    SoftAuthenticator::new(RP, ORIGIN)
}

/// Redeems `invitation` as `name` by registering `browser`'s passkey.
pub async fn redeem(
    f: &Fixture,
    invitation: &str,
    name: &str,
    browser: &SoftAuthenticator,
) -> Result<Redeemed, korpis_app::AppError> {
    let options = f.app.begin_redemption(invitation, name).await?;
    let created = browser.create(&options.challenge);
    let r = NewRegistration {
        challenge: options.challenge_id,
        label: "phone".into(),
        client_data_json: created.client_data_json,
        attestation_object: created.attestation_object,
    };
    f.app.redeem_with_passkey(invitation, (name, r), &client()).await
}

/// The code `secret` gives `ahead` steps from now: a code is accepted a step early, and only
/// for a step later than the last accepted, so a test signing in twice within thirty seconds
/// uses the next step's.
pub fn code(secret: &TotpSecret, ahead: u64) -> String {
    let step = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() / STEP_SECONDS;
    format!("{:06}", secret.code(step + ahead))
}

/// The caller signed in as `account`.
pub fn signed_in(account: Uuid) -> Caller {
    Caller::new(Principal::User(account), None, true)
}

/// A browser's client, from nowhere in particular.
pub fn client() -> Client {
    Client { kind: SessionKind::Web, user_agent: "test".into(), ip: None, devices: Vec::new() }
}
