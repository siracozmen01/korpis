// SPDX-License-Identifier: AGPL-3.0-or-later

//! A recipe's step providers are pinned when it is declared (§4.1 of docs/16-extensions.md): each
//! step kind to the installation that answers it and that installation's artifact digest, each
//! step's arguments checked against the schema it declares, and a kind nothing provides refused.

use crate::recipes::{PAPER, body, create, project, push_paper_like};
use crate::step::serving;
use crate::support::{Harness, detail};

/// The example recipe, its install ending with `step`.
fn with_step(step: &str) -> String {
    let eula = r#"    - write: {path: /data/eula.txt, content: "eula=true\n"}"#;
    assert!(PAPER.contains(eula), "the example installs its eula");
    PAPER.replacen(eula, &format!("{eula}\n    - {step}"), 1)
}

#[tokio::test]
async fn a_step_kind_is_pinned_to_its_installation_and_its_arguments_are_checked() {
    let h = Harness::new().await;
    let steam = serving(&h).await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());

    let repo = push_paper_like(&with_step("valve/steam.app: {to: /data/server, appid: 730}")).await;
    let declared = workloads
        .declare_intent(create("d1", &project, "cs", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let intent = declared.intent.as_option().unwrap();
    let recipe = intent.body.as_option().unwrap().recipe.as_option().unwrap();
    let pins: Vec<_> = recipe
        .providers
        .iter()
        .map(|p| (p.step.as_str(), p.extension.as_str(), p.digest.as_str()))
        .collect();
    let artifact = steam.artifact.as_option().unwrap();
    assert_eq!(pins, [("valve/steam.app", steam.id.as_str(), artifact.digest.as_str())]);

    for (step, why) in [
        ("valve/steam.app: {to: /data/server, appid: cs2}", "an int"),
        ("valve/steam.app: {to: /data/server, appid: 730, beta: true}", "an undeclared argument"),
        ("valve/steam.app: {to: /data/server}", "a required argument missing"),
        ("acme/other.app: {to: /data/server}", "a kind no installation provides"),
    ] {
        let repo = push_paper_like(&with_step(step)).await;
        let e = workloads
            .declare_intent(create(why, &project, "refused", body(&repo, &[])))
            .await
            .unwrap_err();
        let d = detail(&e);
        assert_eq!(d.field, "body.recipe", "{why}: {e:?}");
        assert!(d.message.contains("install.steps[3]"), "{why}: {}", d.message);
    }
}
