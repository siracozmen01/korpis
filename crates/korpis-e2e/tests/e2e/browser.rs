// SPDX-License-Identifier: AGPL-3.0-or-later

//! The panel in a browser (§9 of docs/21-stack.md): a control plane serving the built panel, the
//! state each step begins from made through the API, and Playwright driving Chromium through
//! `web/e2e`, as §38 of docs/23-walkthroughs.md traces people's use of it. Where a node can run
//! here (as root, beside containerd), a workload runs on one for the flows that use it; elsewhere
//! those flows say why they were skipped. It needs the panel built (`web/dist`, which
//! scripts/check-web.sh builds) and the browser installed (`scripts/test-tools.sh`); without
//! either it says so and does not run.

use crate::console::converged;
use crate::node::{Node, live};
use crate::recipes::{VERSION, busybox, push_with};
use crate::support::{ControlPlane, mutation};
use korpis_agent::run::Agent;
use korpis_proto::korpis::v1 as pb;
use korpis_recipe::artifact::File;
use std::path::{Path, PathBuf};

/// The talker's Turkish catalogue (§5.3 of docs/09-recipes.md).
const TR: &str = r#"{"config.fields.greeting.label": "Selamlama",
                     "metadata.description": "Cevap veren konsol"}"#;

/// A recipe whose console greets with its configured greeting and answers every line typed into
/// it, in English and, through its catalogue, Turkish. It ends when asked to, as a program
/// running as a container's first process must say for itself.
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
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu:    {{min: 100m, recommended: 250m}}
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
    /// Issues `actions` on the root organization to a new user named `display_name`, invited;
    /// the invitation.
    async fn invite(&self, display_name: &str, actions: &[&str]) -> String {
        let scope = pb::Scope {
            kind: Some(pb::scope::Kind::Organization(self.root_org.clone())),
            ..Default::default()
        };
        pb::GrantServiceClient::new(self.http(), self.config())
            .issue_grant(pb::IssueGrantRequest {
                mutation: mutation(display_name).into(),
                actions: actions.iter().map(|a| (*a).to_owned()).collect(),
                scope: scope.into(),
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
    let cp = ControlPlane::start_panel(dist).await;
    let invitation = cp.invite("Friend", &["workload.read", "workload.write", "org.read"]).await;
    let operator = cp.invite("Operator", OPERATOR).await;
    let talking = talking(&cp).await;
    let mut playwright = tokio::process::Command::new("pnpm");
    playwright
        .args(["exec", "playwright", "test", "--config", "e2e/playwright.config.ts"])
        .current_dir(web())
        .env("PLAYWRIGHT_BROWSERS_PATH", browsers())
        .env("KORPIS_E2E_URL", &cp.url)
        .env("KORPIS_E2E_INVITATION", invitation)
        .env("KORPIS_E2E_OPERATOR_INVITATION", operator);
    if let Some((_, _, w)) = &talking {
        playwright.env("KORPIS_E2E_WORKLOAD", w);
    }
    let ran = playwright.status().await.unwrap();
    assert!(ran.success(), "the browser tests failed: {ran}");
    if let Some((_, agent, _)) = talking {
        agent.stop().await;
    }
}

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
];
