// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server migrate` and the schema `serve` will run on (§4 of docs/18-operations.md): a
//! rollback undoes what the newer build applied, a schema older or newer than the binary is
//! refused by `serve` with what makes them meet, and nothing is undone past the baseline.

use korpis_store::schema::{BASELINE, latest};
use korpis_store::testing::TestDb;
use sqlx::Connection as _;

fn server(args: &[&str]) -> (bool, String) {
    let done = std::process::Command::new(env!("CARGO_BIN_EXE_korpis-server"))
        .args(args)
        .env_remove("KORPIS_DATABASE_OWNER_URL")
        .output()
        .unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&done.stdout),
        String::from_utf8_lossy(&done.stderr)
    );
    (done.status.success(), said)
}

/// `serve`'s answer about the schema at `url`: it refuses before it reads anything else, and a
/// schema that fits gets as far as the data directory, which is not there.
fn serve(url: &str) -> String {
    let (served, said) = server(&["serve", "--data-dir", "/nonexistent", "--database-url", url]);
    assert!(!served, "{said}");
    said
}

#[tokio::test]
async fn a_rollback_undoes_the_newer_build_s_migrations_and_serve_refuses_a_schema_not_its_own() {
    let db = TestDb::new().await.unwrap();
    let url = db.owner_url.as_str();
    let to = BASELINE.to_string();
    let (ok, said) = server(&["migrate", "--database-owner-url", url, "--to", &to, "--dry-run"]);
    assert!(ok && said.starts_with(&format!("Would undo {:04} ", latest())), "{said}");
    assert!(!serve(url).contains("older than this build"), "a dry run changed nothing");

    let (ok, said) = server(&["migrate", "--database-owner-url", url, "--to", &to]);
    assert!(ok && said.contains("Undid 0075 owner acts as app"), "{said}");
    let refused = serve(url);
    assert!(refused.contains("older than this build") && refused.contains("migrate"), "{refused}");

    let (ok, said) = server(&["migrate", "--database-owner-url", url]);
    assert!(ok && said.contains(&format!("Applied {:04} ", latest())), "{said}");
    assert!(!serve(url).contains("than this build"), "it fits again");
    let (ok, said) = server(&["migrate", "--database-owner-url", url]);
    assert!(ok && said.contains("nothing to do"), "{said}");

    let past = (BASELINE - 1).to_string();
    let (ok, said) = server(&["migrate", "--database-owner-url", url, "--to", &past]);
    assert!(!ok && said.contains("part of the baseline"), "{said}");

    // A newer build's migration, as that build would have recorded it.
    let mut c = sqlx::PgConnection::connect(url).await.unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
         VALUES (999999, 'from a newer build', true, '\\x00', 0)",
    )
    .execute(&mut c)
    .await
    .unwrap();
    let refused = serve(url);
    assert!(refused.contains("newer than this build: migrations 999999"), "{refused}");
    let (ok, said) = server(&["migrate", "--database-owner-url", url, "--to", &to]);
    assert!(!ok && said.contains("undo it with the binary that applied it"), "{said}");
}
