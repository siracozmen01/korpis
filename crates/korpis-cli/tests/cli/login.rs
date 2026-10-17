// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis login` (§7 of docs/13-surface-cli.md): the binary prints a user code, a user signed in
//! to the panel approves it, and the session is kept, in a file readable by its user alone where
//! no keyring is reachable, for every later command to exchange; `korpis logout` ends it; and
//! `--no-store` keeps nothing, running one command with the session in memory.

use crate::support::{ControlPlane, PANEL};
use korpis_cli::connect::{Connection, Endpoint};
use korpis_core::webauthn::testing::SoftAuthenticator;
use korpis_proto::korpis::v1 as pb;
use std::io::{BufRead as _, BufReader};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// The organization `id`, as a grant's scope.
fn org_scope(id: &str) -> pb::Scope {
    use korpis_core::id::{Id, IdKind};
    use korpis_core::object::{Kind, ObjectRef};
    let id = Id::parse(IdKind::Organization, id).unwrap().uuid();
    korpis_wire::scope::to_pb(&korpis_core::scope::Scope::Object(ObjectRef::new(
        Kind::Organization,
        id,
    )))
}

/// A connection to `cp` borne by `token`, or by nothing.
fn connection(cp: &ControlPlane, token: &str) -> Connection {
    let ca = cp.data.tls().join("ca.pem");
    Connection::open(&Endpoint { server: cp.url.clone(), ca, token: token.to_owned() }).unwrap()
}

/// An access token for a user signed in to the panel: invited by root, redeemed with a passkey.
async fn panel_user(cp: &ControlPlane) -> String {
    let root = connection(cp, &cp.secret);
    let issued = pb::GrantServiceClient::new(root.http(), root.config())
        .issue_grant(pb::IssueGrantRequest {
            mutation: pb::Mutation { idempotency_key: "invite".into(), ..Default::default() }
                .into(),
            actions: vec!["workload.read".to_owned()],
            scope: org_scope(&cp.root_org).into(),
            to_new_user: pb::NewUser { display_name: "A Friend".into(), ..Default::default() }
                .into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let anonymous = connection(cp, "");
    let accounts = pb::AccountServiceClient::new(anonymous.http(), anonymous.config());
    let begun = accounts
        .begin_redemption(pb::BeginRedemptionRequest {
            invitation: issued.invitation.clone(),
            name: "friend".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let creation = begun.creation.as_option().unwrap().clone();
    let created = SoftAuthenticator::new("localhost", PANEL).create(&creation.challenge);
    let redeemed = accounts
        .redeem_with_passkey(pb::RedeemWithPasskeyRequest {
            invitation: issued.invitation,
            name: "friend".into(),
            registration: pb::PasskeyRegistration {
                challenge_id: creation.challenge_id,
                label: "phone".into(),
                client_data_json: created.client_data_json,
                attestation_object: created.attestation_object,
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let cookie = redeemed
        .headers()
        .get_all(http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| v.strip_prefix("__Host-korpis_refresh="))
        .and_then(|v| v.split(';').next())
        .unwrap()
        .to_owned();
    let sessions = pb::SessionServiceClient::new(
        anonymous.http(),
        anonymous.config().with_default_header("cookie", format!("__Host-korpis_refresh={cookie}")),
    );
    sessions.exchange(pb::ExchangeRequest::default()).await.unwrap().into_owned().access_token
}

/// `korpis` with `args` against `cp`, with no token, its sessions kept under `dir`.
fn korpis(cp: &ControlPlane, dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_korpis"));
    cmd.args(args)
        .env("KORPIS_SERVER", &cp.url)
        .env("KORPIS_CA", cp.data.tls().join("ca.pem"))
        .env_remove("KORPIS_TOKEN")
        .env("KORPIS_KEYRING", "off")
        .env("KORPIS_CONFIG_DIR", dir);
    cmd
}

/// Runs `cmd`, approving as `panel` the user code it prints, to its end.
async fn approved(cp: &ControlPlane, panel: &str, mut cmd: Command) -> Output {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("korpis runs");
    let stderr = child.stderr.take().unwrap();
    let (told, code) = tokio::sync::oneshot::channel();
    // The code is the first line's last word; the rest of standard error is read until the end.
    let rest = tokio::task::spawn_blocking(move || {
        let mut lines = BufReader::new(stderr).lines();
        let first = lines.next().unwrap().unwrap();
        let _ = told.send(first.rsplit(' ').next().unwrap().to_owned());
        lines.map_while(Result::ok).collect::<Vec<_>>().join("\n")
    });
    let code =
        tokio::time::timeout(std::time::Duration::from_secs(30), code).await.unwrap().unwrap();
    let c = connection(cp, panel);
    pb::SessionServiceClient::new(c.http(), c.config())
        .approve_device(pb::ApproveDeviceRequest { user_code: code, ..Default::default() })
        .await
        .unwrap();
    let mut output =
        tokio::task::spawn_blocking(move || child.wait_with_output().unwrap()).await.unwrap();
    output.stderr = rest.await.unwrap().into_bytes();
    output
}

/// `cmd` run to its end off the runtime, which serves the control plane meanwhile.
async fn run(mut cmd: Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[tokio::test]
async fn a_login_is_approved_in_the_panel_kept_for_later_commands_and_ended_by_logout() {
    let cp = ControlPlane::start().await;
    let panel = panel_user(&cp).await;
    let dir = tempfile::tempdir().unwrap();

    let out = approved(&cp, &panel, korpis(&cp, dir.path(), &["login"])).await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("readable by you alone"), "{}", text(&out.stderr));
    let kept: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path()).collect();
    let session = kept.iter().find(|p| p.extension().is_some_and(|e| e == "session")).unwrap();
    assert_eq!(std::fs::metadata(session).unwrap().permissions().mode() & 0o777, 0o600);

    let listed = run(korpis(&cp, dir.path(), &["call", "SessionService/ListSessions"])).await;
    assert!(listed.status.success(), "{}", text(&listed.stderr));
    assert!(text(&listed.stdout).contains("\"cli\""), "{}", text(&listed.stdout));
    let again = run(korpis(&cp, dir.path(), &["call", "SessionService/ListSessions"])).await;
    assert!(again.status.success(), "the kept session serves the next command too");

    let out = run(korpis(&cp, dir.path(), &["logout"])).await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!session.exists(), "logout forgets the session");
    let after = run(korpis(&cp, dir.path(), &["call", "SessionService/ListSessions"])).await;
    assert!(!after.status.success(), "no session, no credential");
}

#[tokio::test]
async fn no_store_runs_one_command_with_the_session_and_keeps_nothing() {
    let cp = ControlPlane::start().await;
    let panel = panel_user(&cp).await;
    let dir = tempfile::tempdir().unwrap();
    let args = ["login", "--no-store", "--", "call", "SessionService/ListSessions"];
    let out = approved(&cp, &panel, korpis(&cp, dir.path(), &args)).await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("\"cli\""), "{}", text(&out.stdout));
    assert!(std::fs::read_dir(dir.path()).map_or(true, |mut d| d.next().is_none()), "kept nothing");
    let c = connection(&cp, &panel);
    let listed = pb::SessionServiceClient::new(c.http(), c.config())
        .list_sessions(pb::ListSessionsRequest::default())
        .await
        .unwrap()
        .into_owned();
    assert!(listed.sessions.iter().all(|s| s.kind == "web"), "the CLI's session was ended");
}
