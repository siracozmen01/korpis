// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-server install --postgres container` (§2 of docs/18-operations.md): the official image,
//! pinned by digest and published on loopback, given an owner that is not a superuser, and the
//! installation made in it as on any server. Run against this machine's docker, with a container
//! and a port of the test's own, removed when it ends.

use sqlx::Connection as _;

/// The container and its volume, removed however the test ends.
struct Removed(String);

impl Drop for Removed {
    fn drop(&mut self) {
        let docker = |args: &[&str]| std::process::Command::new("docker").args(args).output();
        let _ = docker(&["rm", "-f", &self.0]);
        let _ = docker(&["volume", "rm", &format!("{}-data", self.0)]);
    }
}

#[tokio::test]
async fn a_container_is_made_on_loopback_with_an_owner_that_is_not_a_superuser() {
    let name = format!("korpis-test-pg-{}", uuid::Uuid::now_v7().simple());
    let _removed = Removed(name.clone());
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let dir = tempfile::tempdir().unwrap();
    let shm = tempfile::tempdir_in("/dev/shm").unwrap();
    let done = std::process::Command::new(env!("CARGO_BIN_EXE_korpis-server"))
        .args(["install", "--organization", "acme", "--first-node", "first", "--root-token"])
        .args(["--postgres", "container", "--postgres-container", &name, "--no-system"])
        .args(["--postgres-port", &port.to_string()])
        .arg("--backup-target")
        .arg(shm.path())
        .arg("--data-dir")
        .arg(dir.path().join("data"))
        .env_remove("KORPIS_DATABASE_OWNER_URL")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&done.stdout);
    assert!(done.status.success(), "{out}{}", String::from_utf8_lossy(&done.stderr));
    assert!(out.contains("an evaluation setup, not a production one"), "{out}");

    let published = std::process::Command::new("docker").args(["port", &name]).output().unwrap();
    let published = String::from_utf8_lossy(&published.stdout);
    assert_eq!(published.trim(), format!("5432/tcp -> 127.0.0.1:{port}"), "loopback alone");
    let owner = out.lines().skip_while(|l| !l.starts_with("The owner's URL")).nth(2);
    let owner = owner.and_then(|l| l.trim().strip_prefix("postgres://")).unwrap();
    let mut c = sqlx::PgConnection::connect(&format!("postgres://{owner}")).await.unwrap();
    let (superuser, organizations): (bool, i64) = sqlx::query_as(
        "SELECT rolsuper, (SELECT count(*) FROM organization) FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&mut c)
    .await
    .unwrap();
    assert!(!superuser, "the owner owns the schema and nothing more");
    assert_eq!(organizations, 1, "installed into it");

    let again = std::process::Command::new(env!("CARGO_BIN_EXE_korpis-server"))
        .args(["install", "--organization", "acme", "--postgres", "container", "--no-system"])
        .args(["--postgres-container", &name, "--backup-target"])
        .arg(shm.path())
        .arg("--data-dir")
        .arg(dir.path().join("again"))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&again.stderr);
    assert!(!again.status.success() && said.contains("is there"), "{said}");
}
