// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server install` and `korpis-server admin recover-root` (§9.1 and §9.2 of
//! docs/08-identity.md): by default the root grant is held by an account whose invitation is
//! printed once; `--root-token` prints a token instead; and recovery runs only against an
//! installation's own data directory.

use korpis_core::tenancy::Name;
use korpis_server::data::DataDir;
use korpis_server::install::target::BackupTarget;
use korpis_server::install::{
    InstallConfig, InstallError, RecoverConfig, RootHolder, install, installed_report, recover_root,
};
use korpis_store::testing::TestDb;
use sqlx::Connection as _;

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
        first_node: None,
        backup_target: None,
        database_url: Some(db.owner_url.clone()),
    };
    let done = install(&config).await.unwrap();
    let report = installed_report(&config, &done, "https://localhost:7443");
    assert!(report.contains("korpisiv_"), "{report}");
    assert!(report.contains("registering a passkey"), "{report}");
    assert!(!report.contains("korpis_"), "no token is printed: {report}");
    let facts = korpis_store::server::server_facts(&db.owner_url).await.unwrap();
    assert!(facts.installed, "a second install is refused by preflight");

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
        first_node: Some(Name::parse("first").unwrap()),
        backup_target: Some(BackupTarget::Directory(dir.path().join("elsewhere"))),
        database_url: None,
    };
    let done = install(&config).await.unwrap();
    let report = installed_report(&config, &done, "https://localhost:7443");
    assert!(report.contains(done.installation.token().unwrap().1.expose()), "{report}");
    assert!(report.contains("no second factor"), "{report}");
    let key = done.backup_key.as_deref().unwrap();
    assert!(report.contains(key), "the backup key is shown once: {report}");
    let first = done.installation.first_node.as_ref().unwrap();
    assert!(report.contains(&format!(
        "runuser -u korpis-agent -- korpis-agent join --server https://localhost:7443 \\\n      \
         --token {} --ca-fingerprint {}",
        first.secret.expose(),
        done.ca_fingerprint
    )));

    // The control plane's own login belongs to the application role and owns nothing.
    let mut c = sqlx::PgConnection::connect(&done.database_url).await.unwrap();
    let user: String = sqlx::query_scalar("SELECT current_user").fetch_one(&mut c).await.unwrap();
    assert_eq!(user, "korpis_server");
    sqlx::query("SET ROLE korpis_app").execute(&mut c).await.unwrap();
    assert!(sqlx::query("SELECT count(*) FROM organization").execute(&mut c).await.is_ok());
    let altered = sqlx::query("ALTER TABLE organization ADD COLUMN x int").execute(&mut c).await;
    assert!(altered.is_err(), "it cannot change the schema");
}

#[tokio::test]
async fn the_command_runs_preflight_first_and_changes_nothing_when_it_refuses() {
    let db = TestDb::new().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let run = |more: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_korpis-server"))
            .args(["install", "--organization", "acme", "--first-node", "first"])
            .arg("--data-dir")
            .arg(&data)
            .args(["--database-owner-url", &db.owner_url])
            .args(more)
            .output()
            .unwrap()
    };
    let refused = run(&[]);
    let (out, err) =
        (String::from_utf8_lossy(&refused.stdout), String::from_utf8_lossy(&refused.stderr));
    assert!(!refused.status.success());
    assert!(out.contains("[NO] control-plane backup target: none declared"), "{out}");
    assert!(err.contains("nothing was changed"), "{err}");
    assert!(!data.exists(), "preflight wrote nothing");

    // A tmpfs is another device than the data directory's disk; a dry run writes nothing.
    let shm = tempfile::tempdir_in("/dev/shm").unwrap();
    let dry = run(&["--dry-run", "--backup-target", shm.path().to_str().unwrap()]);
    let out = String::from_utf8_lossy(&dry.stdout);
    assert!(dry.status.success(), "{out}{}", String::from_utf8_lossy(&dry.stderr));
    assert!(out.contains("Would write /etc/sysusers.d/korpis.conf"), "{out}");
    assert!(!data.exists() && !shm.path().join("target").exists());
    let facts = korpis_store::server::server_facts(&db.owner_url).await.unwrap();
    assert!(!facts.installed);
}
