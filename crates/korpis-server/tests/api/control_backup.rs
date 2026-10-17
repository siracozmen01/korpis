// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server backup` (§6 of docs/18-operations.md): every key of the data directory and the
//! epoch watermark, sealed with the key installation printed, in the target it declared, read
//! back whole; and a failure that says so when the database itself is not archived.

use base64::Engine as _;
use korpis_core::tenancy::Name;
use korpis_server::control_backup::{declared, open};
use korpis_server::data::DataDir;
use korpis_server::install::target::BackupTarget;
use korpis_server::install::{InstallConfig, RootHolder, install};
use korpis_store::testing::TestDb;

fn backup(data: &DataDir, url: &str, keep: &str) -> (bool, String, String) {
    let done = std::process::Command::new(env!("CARGO_BIN_EXE_korpis-server"))
        .arg("backup")
        .arg("--data-dir")
        .arg(data.root())
        .args(["--database-url", url, "--keep", keep])
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (done.status.success(), text(&done.stdout), text(&done.stderr))
}

fn kept(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    files
}

#[tokio::test]
async fn the_keys_and_the_watermark_are_sealed_in_the_target_and_an_unarchived_database_is_said() {
    let db = TestDb::new().await.unwrap();
    let login = db.login().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir_in("/dev/shm").unwrap();
    let data = DataDir::new(dir.path().join("data"));
    install(&InstallConfig {
        data: data.clone(),
        owner_url: db.owner_url.clone(),
        organization: Name::parse("acme").unwrap(),
        tls_names: vec!["localhost".to_owned()],
        root: RootHolder::Token,
        first_node: None,
        backup_target: Some(BackupTarget::Directory(target.path().to_owned())),
        database_url: Some(login.clone()),
    })
    .await
    .unwrap();

    let (ok, out, err) = backup(&data, &login, "30");
    assert!(!ok, "the database is not archived here: {out}{err}");
    assert!(out.starts_with("Backed up "), "the keys were, and it says so first: {out}");
    assert!(err.contains("archive_mode is off, so the database is not backed up"), "{err}");

    let files = kept(target.path());
    assert_eq!(files.len(), 1, "{files:?}");
    let (_, key) = declared(&data).unwrap();
    let bundle = open(&std::fs::read(&files[0]).unwrap(), &key).unwrap();
    let watermark = korpis_store::server::epoch_watermark(&db.owner_url).await.unwrap();
    assert_eq!(bundle.epoch, watermark);
    let grant = bundle.files.get("keys/token-1.key").unwrap();
    let on_disk = std::fs::read(data.keys().join("token-1.key")).unwrap();
    assert_eq!(base64::engine::general_purpose::STANDARD.decode(grant).unwrap(), on_disk);
    assert!(bundle.files.contains_key("tls/ca.pem"), "the CA nodes pin");
    assert!(!bundle.files.keys().any(|f| f.starts_with("backup/")), "never its own key");

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (_, out, _) = backup(&data, &login, "1");
    assert!(out.contains(&format!("Removed {}", files[0].display())), "{out}");
    assert_eq!(kept(target.path()).len(), 1, "the newest kept");
}
