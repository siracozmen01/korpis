// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server restore` (§6 of docs/18-operations.md): from the control-plane backup and its
//! key, into a data directory lost with its machine, the keys are put back, the epochs fenced
//! with the window left for the next start, the token key rotated, the relays held, and the gap
//! recorded; a data directory holding a key other than the backup's is refused, changing nothing.

use base64::Engine as _;
use korpis_app::ports::{EnrollmentRead as _, SystemClock};
use korpis_app::{App, NoRecipes};
use korpis_core::authz::token::key_name;
use korpis_server::control_backup::{Bundle, collect, seal};
use korpis_server::data::DataDir;
use korpis_store::server::EpochWatermark;
use korpis_store::testing::TestDb;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn restore(data: &DataDir, login: &str, from: &std::path::Path, key: &str) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_korpis-server"))
        .arg("restore")
        .arg("--data-dir")
        .arg(data.root())
        .args(["--database-url", login, "--key", key, "--settle-secs", "5"])
        .arg("--from")
        .arg(from)
        .output()
        .unwrap()
}

async fn count(db: &TestDb, sql: &str) -> i64 {
    let store = db.store().await.unwrap();
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).fetch_one(store.pool()).await.unwrap()
}

#[tokio::test]
async fn a_machine_lost_with_its_data_directory_is_restored_from_the_backup_and_its_key() {
    let db = TestDb::new().await.unwrap();
    let login = db.login().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let lost = DataDir::new(dir.path().join("lost"));
    korpis_server::testing::install_acme(&db.owner_url, &lost, &login).await;
    let key = [7u8; 32];
    let taken_at = SystemTime::now() - Duration::from_secs(3600);
    let bundle = Bundle {
        taken_at: taken_at.duration_since(UNIX_EPOCH).unwrap().as_secs(),
        epoch: EpochWatermark { max_issued: 7, issue_rate_per_sec: 2 },
        files: collect(&lost).unwrap(),
    };
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let from = target.join("korpis-control-plane-1.bin");
    std::fs::write(&from, seal(&bundle, &key).unwrap()).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(key);

    let fresh = DataDir::new(dir.path().join("fresh"));
    std::fs::create_dir(fresh.root()).unwrap();
    let wrong = base64::engine::general_purpose::STANDARD.encode([8u8; 32]);
    let refused = restore(&fresh, &login, &from, &wrong);
    assert!(!refused.status.success(), "another key opens nothing");
    let done = restore(&fresh, &login, &from, &encoded);
    let said = String::from_utf8_lossy(&done.stdout);
    assert!(done.status.success(), "{said} {}", String::from_utf8_lossy(&done.stderr));
    assert!(said.contains(&format!("Keys: {} put back", bundle.files.len())), "{said}");
    assert!(said.contains("backup declaration was missing"), "{said}");
    let declared = std::fs::read_to_string(fresh.backup().join("target")).unwrap();
    assert_eq!(declared, target.canonicalize().unwrap().display().to_string());

    let floor = count(&db, "SELECT floor FROM epoch_state").await;
    assert!(floor >= 7 + 2 * 3601, "past every epoch the rate allowed since the backup: {floor}");
    assert_eq!(count(&db, "SELECT settle_pending_ms FROM epoch_state").await, 5000, "pending");
    let store = db.store().await.unwrap();
    let keys: Vec<String> =
        store.verify_keys("token").await.unwrap().into_iter().map(|k| k.key_id).collect();
    assert_eq!(keys, [key_name(2)], "rotated past the backup's");
    for action in
        ["restore.epochs.fenced", "restore.token_key.rotated", "restore.relays.held", "restore.gap"]
    {
        let n = count(&db, &format!("SELECT count(*) FROM effect WHERE action = '{action}'")).await;
        assert_eq!(n, 1, "{action}");
    }

    // The next start opens the window, once.
    let app = App::new(store, SystemClock, test_key(), NoRecipes);
    let opened = app.open_pending_settle().await.unwrap().expect("a window was pending");
    assert!(opened > SystemTime::now(), "open from now");
    assert!(app.open_pending_settle().await.unwrap().is_none(), "once");

    // A data directory holding a key other than the backup's: refused, nothing changed.
    let token = fresh.keys().join("token-1.key");
    let mut other = std::fs::read(&token).unwrap();
    other[0] ^= 0xff;
    std::fs::write(&token, &other).unwrap();
    let differs = restore(&fresh, &login, &from, &encoded);
    let told = String::from_utf8_lossy(&differs.stderr);
    assert!(!differs.status.success() && told.contains("differs from the backup's"), "{told}");
    assert_eq!(count(&db, "SELECT count(*) FROM effect WHERE action = 'restore.gap'").await, 1);
}

fn test_key() -> std::sync::Arc<korpis_core::authz::token::SigningKey> {
    std::sync::Arc::new(korpis_core::authz::token::SigningKey::generate(9))
}
