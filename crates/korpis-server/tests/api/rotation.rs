// SPDX-License-Identifier: AGPL-3.0-or-later

//! A restore's second step (§8 of docs/03-state.md): the token key rotated, so every token signed
//! before stops verifying and no verifier is given the old key again, and every session ended.

use korpis_app::ports::EnrollmentRead as _;
use korpis_core::authz::token::key_name;
use korpis_server::data::DataDir;
use korpis_server::install::RecoverConfig;
use korpis_server::keys::Keys;
use korpis_server::restore::rotate_token_key;
use korpis_store::testing::TestDb;

#[tokio::test]
async fn a_rotation_retires_every_earlier_token_key_and_ends_every_session() {
    let db = TestDb::new().await.unwrap();
    let login = db.login().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path().join("data"));
    korpis_server::testing::install_acme(&db.owner_url, &data, &login).await;
    let store = db.store().await.unwrap();
    let account = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO account (id, display_name, created_by) VALUES ($1, 'ada', 'system')")
        .bind(account)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO user_session (id, account_id, kind, refresh_hash, created_at, last_used_at,
                                   authenticated_at, expires_at, user_agent, source)
         VALUES ($1, $2, 'web', $3, now(), now(), now(), now() + interval '1 day', 'test', 'test')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(account)
    .bind(vec![7u8; 32])
    .execute(store.pool())
    .await
    .unwrap();
    let before = Keys::load_or_create(&data).unwrap();
    // A key of the next epoch's name the restored store already holds, from a rotation after
    // the backup: the new key takes a name past it, never its name.
    sqlx::query("INSERT INTO signing_key (key_id, purpose, public_key) VALUES ($1, 'token', $2)")
        .bind(key_name(2))
        .bind(vec![9u8; 32])
        .execute(store.pool())
        .await
        .unwrap();

    let config = RecoverConfig { data: data.clone(), database_url: login.clone() };
    let (epoch, rotated) = rotate_token_key(&config, "uid 0 (root)").await.unwrap();
    assert_eq!(epoch, 3, "past every epoch the data directory or the store holds");
    assert_eq!((rotated.retired, rotated.sessions_ended), (2, 1));

    let after = Keys::load_or_create(&data).unwrap();
    assert_ne!(after.token.public_bytes(), before.token.public_bytes(), "signed with the new");
    let given: Vec<String> =
        store.verify_keys("token").await.unwrap().into_iter().map(|k| k.key_id).collect();
    assert_eq!(given, [key_name(3)], "no verifier is given a retired key");
    let assignment = store.verify_keys("assignment").await.unwrap();
    assert_eq!(assignment.len(), 1, "the assignment key is kept, so nodes' assignments verify");
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM user_session")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(sessions, 0, "everyone signs in again");

    // A data directory behind the store: serving would sign with a key no verifier holds.
    std::fs::remove_file(data.keys().join("token-3.key")).unwrap();
    let served = std::process::Command::new(env!("CARGO_BIN_EXE_korpis-server"))
        .arg("serve")
        .arg("--data-dir")
        .arg(data.root())
        .args(["--database-url", &login, "--listen", "127.0.0.1:0"])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&served.stderr);
    assert!(!served.status.success(), "{said}");
    assert!(said.contains("the store trusts token keys"), "{said}");
}
