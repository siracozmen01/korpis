// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server install` and `korpis-server admin recover-root` (§9.1 and §9.2 of
//! docs/08-identity.md): by default the root grant is held by an account whose invitation is
//! printed once; `--root-token` prints a token instead; and recovery runs only against an
//! installation's own data directory.

use korpis_core::tenancy::Name;
use korpis_server::data::DataDir;
use korpis_server::install::{
    InstallConfig, InstallError, RecoverConfig, RootHolder, install, installed_report, recover_root,
};
use korpis_store::testing::TestDb;

#[tokio::test]
async fn an_install_prints_the_root_account_s_invitation_and_recovery_prints_another() {
    let db = TestDb::new().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = InstallConfig {
        data: DataDir::new(dir.path().join("data")),
        owner_url: db.owner_url.clone(),
        organization: Name::parse("acme").unwrap(),
        tls_names: vec!["localhost".to_owned()],
        root: RootHolder::Passkey,
    };
    let done = install(&config).await.unwrap();
    let report = installed_report(&config, &done);
    assert!(report.contains("korpisiv_"), "{report}");
    assert!(report.contains("registering a passkey"), "{report}");
    assert!(!report.contains("korpis_"), "no token is printed: {report}");

    let elsewhere = RecoverConfig {
        data: DataDir::new(dir.path().join("not-an-installation")),
        database_url: db.owner_url.clone(),
    };
    let refused = recover_root(&elsewhere, "uid 0 (root)").await.unwrap_err();
    assert!(matches!(refused, InstallError::NotInstalled(_)), "{refused}");

    let here = RecoverConfig { data: config.data.clone(), database_url: db.owner_url.clone() };
    let recovered = recover_root(&here, "uid 0 (root)").await.unwrap();
    assert!(recovered.secret.expose().starts_with("korpisiv_"));
}

#[tokio::test]
async fn an_install_with_a_root_token_prints_the_token_and_says_what_it_lacks() {
    let db = TestDb::new().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = InstallConfig {
        data: DataDir::new(dir.path().join("data")),
        owner_url: db.owner_url.clone(),
        organization: Name::parse("acme").unwrap(),
        tls_names: vec!["localhost".to_owned()],
        root: RootHolder::Token,
    };
    let done = install(&config).await.unwrap();
    let report = installed_report(&config, &done);
    assert!(report.contains(done.token().unwrap().1.expose()), "{report}");
    assert!(report.contains("no second factor"), "{report}");
}
