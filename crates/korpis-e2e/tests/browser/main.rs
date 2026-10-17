// SPDX-License-Identifier: AGPL-3.0-or-later

//! The panel in a browser (§9 of docs/21-stack.md): a control plane serving the built panel, the
//! state each step begins from made through the API, and Playwright driving Chromium through
//! `web/e2e`, as §38 of docs/23-walkthroughs.md traces people's use of it. Where a node can run
//! here (as root, beside containerd), a workload runs on one for the flows that use it; elsewhere
//! those flows say why they were skipped. It needs the panel built (`web/dist`, which
//! scripts/check-web.sh builds) and the browser installed (`scripts/test-tools.sh`); without
//! either it says so and does not run.
//!
//! It is a test binary of its own, which cargo runs alone: the node tests beside it in `e2e` add
//! and remove network interfaces, and Chromium abandons every request in flight when the
//! machine's addresses change (Finding 88 of docs/23-walkthroughs.md), faster than a person's
//! network ever changes. The modules it shares with `e2e` are that binary's own files.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]
#![allow(dead_code, reason = "the shared modules hold what the other e2e tests use as well")]

#[path = "../e2e/image.rs"]
mod image;
#[path = "../e2e/node.rs"]
mod node;
#[path = "../e2e/recipes.rs"]
mod recipes;
#[path = "../e2e/support.rs"]
mod support;

use crate::node::{Node, live};
use crate::recipes::{VERSION, busybox, push_with};
use crate::support::korpis;
use crate::support::{ControlPlane, converged, mutation};
use korpis_agent::run::Agent;
use korpis_proto::korpis::v1 as pb;
use korpis_recipe::artifact::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, BufReader};

/// The talker's Turkish catalogue (§5.3 of docs/09-recipes.md).
const TR: &str = r#"{"config.fields.greeting.label": "Selamlama",
                     "metadata.description": "Cevap veren konsol"}"#;

/// A recipe whose console greets with its configured greeting and answers every line typed into
/// it, in English and, through its catalogue, Turkish. It ends when asked to, as a program
/// running as a container's first process must say for itself, and says that restarting it
/// costs something (§5.1 of docs/09-recipes.md), as a server with people on it does.
fn talker() -> String {
    let busybox = busybox();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: talker, version: "{VERSION}", description: Answers its console, license: MIT}}
runtime:
  targets:
    - {{tier: container, arch: [amd64, arm64], image: "{busybox}"}}
  entry:
    command:
      - /bin/sh
      - -c
      - "trap 'exit 0' TERM; echo ready $GREETING; while read line; do echo got $line; done"
preset:
  lifecycle: persistent
  console: interactive
  volumes:
    - {{name: data, path: /data, size_default: 64Mi}}
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu:    {{min: 100m, recommended: 250m}}
disruption:
  restart: {{disruptive: true, reason: "drops whoever is attached to its console"}}
config:
  fields:
    - {{key: greeting, label: Greeting, type: string, default: hello, deliver: env,
       deliver_as: GREETING}}
"#
    )
}

/// A node running the talker in the project `games`, converged; the node, its agent, and the
/// workload. `None` where no node can run here.
async fn talking(cp: &ControlPlane) -> Option<(Node, Agent, String)> {
    if !live() {
        return None;
    }
    let catalogue = File {
        path: "i18n/tr.json".to_owned(),
        media_type: "application/json".to_owned(),
        data: TR.as_bytes().to_vec(),
    };
    let repo = push_with("talker", talker(), vec![catalogue]).await;
    let project = cp.project().await;
    let node = Node::join(cp, "fra-3").await;
    let agent = node.agent().await;
    let w = cp.declare(&project, "talker", &repo).await;
    node.guard(&w);
    converged(cp, &w).await;
    Some((node, agent, w))
}

fn web() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web")
}

fn browsers() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tools/ms-playwright")
}

impl ControlPlane {
    /// Issues `actions` on the root organization to a new user named `display_name`, invited,
    /// who may delegate `shares` of them once, as a share link does; the invitation.
    async fn invite(&self, display_name: &str, actions: &[&str], shares: &[&str]) -> String {
        let owned = |list: &[&str]| list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        let delegation = pb::DelegationPolicy {
            allowed: !shares.is_empty(),
            max_depth: Some(u32::from(!shares.is_empty())),
            actions: owned(shares),
            ..Default::default()
        };
        let scope = pb::Scope {
            kind: Some(pb::scope::Kind::Organization(self.root_org.clone())),
            ..Default::default()
        };
        pb::GrantServiceClient::new(self.http(), self.config())
            .issue_grant(pb::IssueGrantRequest {
                mutation: mutation(display_name).into(),
                actions: owned(actions),
                scope: scope.into(),
                delegation: delegation.into(),
                to_new_user: pb::NewUser {
                    display_name: display_name.to_owned(),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_owned()
            .invitation
    }
}

#[tokio::test]
async fn the_panel_is_used_in_a_browser_against_a_real_control_plane() {
    let dist = web().join("dist");
    if !dist.join("index.html").exists() || !browsers().exists() {
        eprintln!(
            "the panel is not built or no browser is installed: scripts/check-web.sh and scripts/test-tools.sh"
        );
        return;
    }
    let cp = Arc::new(ControlPlane::start_panel(dist).await);
    let invitation = cp.invite("Friend", FRIEND, &[]).await;
    let operator =
        cp.invite("Operator", OPERATOR, &["workload.read", "workload.console.read"]).await;
    let keeper = cp.invite("Keeper", &["org.read", "org.write"], &[]).await;
    let talking = talking(&cp).await;
    let config = tempfile::tempdir().unwrap();
    let (login, approve_at) = logging_in(&cp, config.path()).await;
    let mut playwright = tokio::process::Command::new("pnpm");
    playwright
        .args(["exec", "playwright", "test", "--config", "e2e/playwright.config.ts"])
        .current_dir(web())
        .env("PLAYWRIGHT_BROWSERS_PATH", browsers())
        .env("KORPIS_E2E_URL", &cp.url)
        .env("KORPIS_E2E_INVITATION", invitation)
        .env("KORPIS_E2E_OPERATOR_INVITATION", operator)
        .env("KORPIS_E2E_DEVICE_URL", approve_at)
        .env("KORPIS_E2E_KEEPER_INVITATION", keeper)
        .env("KORPIS_E2E_AGE", config.path().join("age"));
    if let Some((_, _, w)) = &talking {
        playwright.env("KORPIS_E2E_WORKLOAD", w);
    }
    let ageing = tokio::spawn(ager(Arc::clone(&cp), config.path().join("age")));
    let ran = playwright.status().await.unwrap();
    ageing.abort();
    assert!(ran.success(), "the browser tests failed: {ran}");
    let logged = tokio::time::timeout(Duration::from_secs(30), login.wait_with_output());
    let logged = logged.await.expect("korpis login ended once approved").unwrap();
    let said = String::from_utf8_lossy(&logged.stdout);
    let told = String::from_utf8_lossy(&logged.stderr);
    assert!(logged.status.success(), "korpis login: {said} {told}");
    assert!(said.contains("friend"), "signed in as the friend who approved it: {said}");
    if let Some((node, agent, w)) = talking {
        let uploaded = std::fs::read(node.volumes.join(format!("{w}.data/world.bin"))).unwrap();
        assert!(uploaded == world(), "the resumed upload is the file, whole and in order");
        agent.stop().await;
    }
}

/// The keeper's browser sessions, signed in eleven minutes earlier than they were.
const AGE_THE_KEEPER: &str = "UPDATE user_session \
    SET authenticated_at = authenticated_at - interval '11 minutes' \
    WHERE kind = 'web' AND account_id = (SELECT id FROM account WHERE name = 'keeper')";

/// Ages the keeper's browser sessions by eleven minutes whenever a browser test asks, by writing
/// the file `asked`, and says it is done by removing it: a session is recent for ten (§9.4 of
/// docs/08-identity.md), and a test that waited that long for it would be a slow test.
async fn ager(cp: Arc<ControlPlane>, asked: PathBuf) {
    loop {
        if asked.exists() {
            cp.execute(AGE_THE_KEEPER).await;
            std::fs::remove_file(&asked).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The file the browser uploads, as `web/e2e/workload.spec.ts` makes it: five mebibytes no two
/// of whose chunks are alike, so a chunk out of place or missing cannot pass for the file.
fn world() -> Vec<u8> {
    (0..5u32 << 20).map(|i| u8::try_from((i * 7 + i / 1_048_576) % 251).unwrap_or(0)).collect()
}

/// `korpis login`, keeping nothing, run to read the caller's profile once signed in, its
/// sessions under `config`; the process, and the address its user code is approved at.
async fn logging_in(cp: &ControlPlane, config: &Path) -> (tokio::process::Child, String) {
    let mut child = tokio::process::Command::new(korpis())
        .args(["login", "--no-store", "--", "call", "ProfileService/GetProfile"])
        .env("KORPIS_SERVER", &cp.url)
        .env("KORPIS_CA", cp.data.tls().join("ca.pem"))
        .env_remove("KORPIS_TOKEN")
        .env("KORPIS_KEYRING", "off")
        .env("KORPIS_CONFIG_DIR", config)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // "To sign in, open <uri> and enter the code <code>", then "or open <uri with the code>".
    let mut told = BufReader::new(child.stderr.take().unwrap()).lines();
    let read = async {
        while let Some(line) = told.next_line().await.unwrap() {
            if let Some(at) = line.strip_prefix("or open ") {
                return at.to_owned();
            }
        }
        panic!("korpis login named no address to approve it at");
    };
    let at = tokio::time::timeout(Duration::from_secs(30), read).await.unwrap();
    (child, at)
}

/// What the friend of the account flows may do, on the root organization.
const FRIEND: &[&str] = &["org.read", "workload.read", "workload.write"];

/// What the operator of the workload flows may do, on the root organization.
const OPERATOR: &[&str] = &[
    "org.read",
    "project.read",
    "workload.read",
    "workload.write",
    "workload.restart",
    "workload.delete",
    "workload.console.read",
    "workload.console.write",
    "workload.file.read",
    "workload.file.write",
    "grant.read",
];
