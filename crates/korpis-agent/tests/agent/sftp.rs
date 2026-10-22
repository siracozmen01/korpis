// SPDX-License-Identifier: AGPL-3.0-or-later

//! SFTP at a relay of its own (§3.1 of docs/02-architecture.md, §3.2 of docs/06-storage.md),
//! driven by OpenSSH's own `sftp` client against a control plane and a node: a person whose key
//! is registered and whose grant allows writing uploads, lists, downloads, renames, and removes,
//! a file of several mebibytes crossing with many requests in flight; a person allowed to read
//! downloads and is refused an upload; a key no one registered is refused; and each token the
//! relay asked for is recorded under the relay's name. Skipped where OpenSSH's client is absent.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::data::files::{READ, WRITE};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::v1 as pb;
use korpis_server::sftp::host_key::host_key;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Whether `program` can be run.
fn have(program: &str) -> bool {
    std::process::Command::new(program).arg("-?").output().is_ok()
}

/// A new key pair at `path`; its public line.
async fn keygen(path: &Path) -> String {
    let made = tokio::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "test", "-f"])
        .arg(path)
        .status()
        .await
        .unwrap();
    assert!(made.success());
    std::fs::read_to_string(path.with_extension("pub")).unwrap()
}

/// OpenSSH's client, in `dir`, as the workload `w` at `port` with `key`, trusting only the host
/// key `known` names, running `batch`; whether it succeeded, and what it said.
async fn sftp(dir: &Path, (w, port): (&str, u16), key: &Path, batch: &str) -> (bool, String) {
    std::fs::write(dir.join("batch"), batch).unwrap();
    let known = format!("UserKnownHostsFile={}", dir.join("known_hosts").display());
    let out = tokio::process::Command::new("sftp")
        .current_dir(dir)
        .args(["-F", "/dev/null", "-b", "batch", "-P", &port.to_string(), "-i"])
        .arg(key)
        .args(["-o", "StrictHostKeyChecking=yes", "-o", &known, "-o", "IdentitiesOnly=yes"])
        .args(["-o", "IdentityAgent=none", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20"])
        .arg(format!("{w}@127.0.0.1"))
        .output()
        .await
        .unwrap();
    let said =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), said)
}

#[tokio::test]
async fn openssh_s_client_reaches_a_workload_s_files_through_a_relay_by_a_registered_key() {
    if !have("sftp") || !have("ssh-keygen") {
        eprintln!("skipped: OpenSSH's sftp and ssh-keygen are not installed");
        return;
    }
    let cp = ControlPlane::start_as(false, |c| c).await;
    let relay = cp.standalone_relay("sftp-1").await;
    let dir = tempfile::tempdir().unwrap();
    let (token, ca) = cp.token("fra-20").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    host.serve_files_from(&dir.path().join("files"));
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let agent = Agent::start(&state, runtime, host.clone(), Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    let volume = pb::VolumeSpec { name: "data".into(), path: "/data".into(), ..Default::default() };
    let w = cp.declare_with(&cp.project().await, "mc", vec![volume]).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let key = host_key(&dir.path().join("sftp_host_key")).unwrap();
    let line = key.public_key().to_openssh().unwrap();
    std::fs::write(dir.path().join("known_hosts"), format!("[127.0.0.1]:{port} {line}\n")).unwrap();
    let _sftp = relay.serve_sftp(listener, key);

    let ada = dir.path().join("ada");
    cp.person_with_key("ada", &[WRITE], &keygen(&ada).await).await;
    let big: Vec<u8> =
        (0..3 * 1024 * 1024 + 17).map(|i: u32| u8::try_from(i % 251).unwrap()).collect();
    std::fs::write(dir.path().join("big.bin"), &big).unwrap();
    std::fs::write(dir.path().join("hello.txt"), b"hello over sftp").unwrap();
    let batch = "put big.bin /data/big.bin\nput hello.txt /data/up.txt\nls -l /data\n\
                 get /data/big.bin back.bin\nmkdir /data/sub\nrename /data/up.txt /data/sub/moved.txt\n\
                 ls /data/sub\n-chmod 600 /data/sub/moved.txt\nget /data/sub/moved.txt moved.txt\n\
                 rm /data/sub/moved.txt\nrmdir /data/sub\nls /data\n";
    let (ok, said) = sftp(dir.path(), (&w, port), &ada, batch).await;
    assert!(ok, "{said}");
    assert!(said.contains("-rw-r--r--") && said.contains("up.txt"), "ls -l: {said}");
    assert!(said.contains("moved.txt"), "{said}");
    assert_eq!(std::fs::read(dir.path().join("back.bin")).unwrap(), big, "every byte, in order");
    assert_eq!(std::fs::read(dir.path().join("moved.txt")).unwrap(), b"hello over sftp");

    let bea = dir.path().join("bea");
    cp.person_with_key("bea", &[READ], &keygen(&bea).await).await;
    let (ok, said) = sftp(dir.path(), (&w, port), &bea, "get /data/big.bin again.bin\n").await;
    assert!(ok, "reading is allowed: {said}");
    let (ok, said) = sftp(dir.path(), (&w, port), &bea, "put hello.txt /data/no.txt\n").await;
    assert!(!ok && said.contains("ermission denied"), "writing is not: {said}");

    let nobody = dir.path().join("nobody");
    keygen(&nobody).await;
    let (ok, said) = sftp(dir.path(), (&w, port), &nobody, "ls /data\n").await;
    assert!(!ok && said.contains("ermission denied"), "a key no one holds: {said}");

    let recorded: i64 = cp
        .scalar(
            "SELECT count(*) FROM effect WHERE action = 'workload.file.sftp_authorized' \
             AND after->>'relay' <> 'control-plane'",
        )
        .await;
    assert_eq!(recorded, 3, "each connection's token, under the relay's name");
    agent.stop().await;
}
