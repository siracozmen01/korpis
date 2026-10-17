// SPDX-License-Identifier: AGPL-3.0-or-later

//! The panel in a browser (§10 of docs/11-surface-web.md): a control plane serving the built
//! panel, the state each step begins from made through the API, and Playwright driving Chromium
//! through `web/e2e`, as §38 of docs/23-walkthroughs.md traces one person's use of it. It needs
//! the panel built (`web/dist`, which scripts/check-web.sh builds) and the browser installed
//! (`scripts/test-tools.sh`); without either it says so and does not run.

use crate::support::{ControlPlane, mutation};
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};

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
    let ran = tokio::process::Command::new("pnpm")
        .args(["exec", "playwright", "test", "--config", "e2e/playwright.config.ts"])
        .current_dir(web())
        .env("PLAYWRIGHT_BROWSERS_PATH", browsers())
        .env("KORPIS_E2E_URL", &cp.url)
        .env("KORPIS_E2E_INVITATION", invitation)
        .status()
        .await
        .unwrap();
    assert!(ran.success(), "the browser tests failed: {ran}");
}
