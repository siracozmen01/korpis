// SPDX-License-Identifier: AGPL-3.0-or-later

//! SFTP on a real node (§3.1 of docs/02-architecture.md, §3.2 of docs/06-storage.md, trace 50 of
//! docs/23-walkthroughs.md), served by the control plane's own relay role and driven by OpenSSH's
//! `sftp`: a person whose key is registered uploads part of a world, resumes it with `reput` from
//! the size the file has, lists it, and downloads it whole; and a connection whose key is removed
//! meanwhile is ended when its token next renews, within one token's lifetime. It needs root,
//! containerd, and OpenSSH's client, and is skipped without them.

use crate::node::{Node, live};
use crate::recipes::push_sleeper;
use crate::support::ControlPlane;
use korpis_core::account::ssh_key::SshPublicKey;
use korpis_core::authz::token::SESSION_TOKEN_LIFETIME;
use korpis_core::relay::sftp::SftpAddress;
use korpis_proto::korpis::v1 as pb;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

const WRITE: &str = "workload.file.write";
const MIB: usize = 1 << 20;

pub fn have(program: &str) -> bool {
    std::process::Command::new(program).arg("-?").output().is_ok()
}

/// SFTP served by `cp`'s own relay role on a port of its own; the port, with the `known_hosts`
/// line `ServerInfo` names written in `dir`.
pub async fn serve_sftp(cp: &ControlPlane, dir: &Path) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let key = korpis_server::sftp::host_key::host_key(&dir.join("sftp_host_key")).unwrap();
    let address = SftpAddress::parse(&format!("127.0.0.1:{port}")).unwrap();
    let serving = cp.api.serve_sftp((listener, key), Vec::new().into(), address).unwrap();
    drop(tokio::spawn(serving));
    let info = pb::ServerInfoServiceClient::new(cp.http(), cp.config());
    let listed = info.get_server_info(pb::GetServerInfoRequest::default()).await.unwrap();
    let own = listed.into_owned().sftp.into_iter().next().expect("the control plane's own");
    assert!(own.relay.is_empty(), "served by the control plane's relay role");
    let line = format!("{} {}\n", own.known_hosts_name, own.host_key);
    std::fs::write(dir.join("known_hosts"), line).unwrap();
    port
}

/// A person invited with `action` on the root organization; their account.
async fn person(cp: &ControlPlane, action: &str) -> uuid::Uuid {
    use korpis_core::authz::{Kind, ObjectRef};
    use korpis_core::id::{Id, IdKind};
    let org = Id::parse(IdKind::Organization, &cp.root_org).unwrap().uuid();
    let scope = korpis_core::scope::Scope::Object(ObjectRef::new(Kind::Organization, org));
    let issued = pb::GrantServiceClient::new(cp.http(), cp.config())
        .issue_grant(pb::IssueGrantRequest {
            mutation: pb::Mutation { idempotency_key: "sftp-person".into(), ..Default::default() }
                .into(),
            actions: vec![action.to_owned()],
            scope: korpis_wire::scope::to_pb(&scope).into(),
            to_new_user: pb::NewUser { display_name: "Ada".into(), ..Default::default() }.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let subject = issued.grant.into_option().unwrap().subject.into_option().unwrap();
    let Some(pb::subject::Kind::User(user)) = subject.kind else { panic!("a person") };
    Id::parse(IdKind::User, &user).unwrap().uuid()
}

/// A person granted `workload.file.write` on the root organization, holding the key made at
/// `path`, which is written to the store as the profile registers one.
pub async fn person_with_key(cp: &ControlPlane, path: &Path) {
    let made = tokio::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .status()
        .await
        .unwrap();
    assert!(made.success());
    let account = person(cp, WRITE).await;
    let key = SshPublicKey::parse(&std::fs::read_to_string(path.with_extension("pub")).unwrap());
    let key = key.unwrap();
    sqlx::query(
        "INSERT INTO ssh_key (id, account_id, kind, public_key, fingerprint, label, added_at)
         VALUES ($1, $2, $3, $4, $5, 'laptop', now())",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(account)
    .bind(key.kind().name())
    .bind(key.blob().to_vec())
    .bind(key.fingerprint())
    .execute(cp.api.app().store().pool())
    .await
    .unwrap();
}

/// OpenSSH's client as `w` at `port` with `key`, in `dir`, reading its commands from standard
/// input, trusting only the host key `ServerInfo` named.
fn sftp(dir: &Path, (w, port): (&str, u16), key: &Path) -> tokio::process::Child {
    let known = format!("UserKnownHostsFile={}", dir.join("known_hosts").display());
    tokio::process::Command::new("sftp")
        .current_dir(dir)
        .args(["-F", "/dev/null", "-b", "-", "-P", &port.to_string(), "-i"])
        .arg(key)
        .args(["-o", "StrictHostKeyChecking=yes", "-o", &known, "-o", "IdentitiesOnly=yes"])
        .args(["-o", "IdentityAgent=none", "-o", "BatchMode=yes"])
        .arg(format!("{w}@127.0.0.1"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

/// `batch` run by OpenSSH's client; whether it succeeded, and what it said.
pub async fn run(dir: &Path, at: (&str, u16), key: &Path, batch: &str) -> (bool, String) {
    let mut child = sftp(dir, at, key);
    child.stdin.take().unwrap().write_all(batch.as_bytes()).await.unwrap();
    let out = child.wait_with_output().await.unwrap();
    let said = [out.stdout, out.stderr].concat();
    (out.status.success(), String::from_utf8_lossy(&said).into_owned())
}

#[tokio::test]
async fn openssh_s_sftp_resumes_an_upload_and_a_removed_key_ends_its_session() {
    if !live() || !have("sftp") {
        return;
    }
    let cp = ControlPlane::start().await;
    let node = Node::join(&cp, "fra-9").await;
    let agent = node.agent().await;
    let w = cp.declare(&cp.project().await, "world", &push_sleeper().await).await;
    node.guard(&w);
    let here = tempfile::tempdir().unwrap();
    let port = serve_sftp(&cp, here.path()).await;
    let key = here.path().join("id");
    person_with_key(&cp, &key).await;

    // Part of a world is uploaded, as a connection lost part way leaves it, then resumed.
    let world: Vec<u8> = (0..5 * MIB + 7).map(|i| u8::try_from(i % 251).unwrap()).collect();
    std::fs::write(here.path().join("part.bin"), &world[..2 * MIB]).unwrap();
    std::fs::write(here.path().join("world.bin"), &world).unwrap();
    let batch = "put part.bin /data/world.bin\nreput world.bin /data/world.bin\nls -l /data\n\
                 get /data/world.bin back.bin\n";
    let (ok, said) = run(here.path(), (&w, port), &key, batch).await;
    assert!(ok, "{said}");
    assert!(said.contains("world.bin"), "listed: {said}");
    assert_eq!(std::fs::read(here.path().join("back.bin")).unwrap(), world, "whole, in order");

    // A connection held open while its key is removed ends when its token next renews.
    let mut held = sftp(here.path(), (&w, port), &key);
    let mut stdin = held.stdin.take().unwrap();
    let mut out = BufReader::new(held.stdout.take().unwrap()).lines();
    stdin.write_all(b"ls /data\n").await.unwrap();
    while let Some(line) = out.next_line().await.unwrap() {
        if line.contains("world.bin") {
            break;
        }
    }
    let removed = sqlx::query("DELETE FROM ssh_key").execute(cp.api.app().store().pool()).await;
    assert_eq!(removed.unwrap().rows_affected(), 1);
    tokio::time::sleep(SESSION_TOKEN_LIFETIME + Duration::from_secs(10)).await;
    let _ = stdin.write_all(b"ls /data\n").await;
    drop(stdin);
    let ended = tokio::time::timeout(Duration::from_secs(30), held.wait()).await.unwrap().unwrap();
    assert!(!ended.success(), "the session ended within one token lifetime of the key's removal");
    drop(agent);
}
