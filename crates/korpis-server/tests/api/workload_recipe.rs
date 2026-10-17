// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's recipe as a client shows it (§5.3 of docs/09-recipes.md): its schema, and its
//! messages in the first of the caller's languages it speaks, read under `workload.read`.

use crate::recipes::{PAPER, body, create, project, push_paper_with};
use crate::support::Harness;
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use korpis_recipe::artifact::File;

const TR: &str = r#"{"config.fields.max_players.label": "En fazla oyuncu",
                     "metadata.description": "PaperMC sunucusu"}"#;

async fn workload(h: &Harness) -> String {
    let catalogue = File {
        path: "i18n/tr.json".to_owned(),
        media_type: "application/json".to_owned(),
        data: TR.as_bytes().to_vec(),
    };
    let repo = push_paper_with(PAPER, vec![catalogue]).await;
    let project = project(h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("d", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    declared.workload.as_option().unwrap().id.clone()
}

fn asking(workload: &str, locales: &[&str]) -> pb::GetWorkloadRecipeRequest {
    pb::GetWorkloadRecipeRequest {
        workload: workload.to_owned(),
        locales: locales.iter().map(|l| (*l).to_owned()).collect(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_workload_s_recipe_speaks_the_first_language_it_has_of_the_caller_s() {
    let h = Harness::new().await;
    let w = workload(&h).await;
    let read = |locales: &'static [&'static str]| {
        let client = h.workloads(h.root());
        let w = w.clone();
        async move { client.get_workload_recipe(asking(&w, locales)).await.unwrap().into_owned() }
    };
    let tr = read(&["tr-TR", "en"]).await;
    assert_eq!(tr.locale, "tr");
    assert_eq!(tr.messages["config.fields.max_players.label"], "En fazla oyuncu");
    assert_eq!(tr.messages["metadata.description"], "PaperMC sunucusu");
    let schema = tr.config.as_option().unwrap();
    let field = schema.fields.iter().find(|f| f.key == "max_players").unwrap();
    assert_eq!(field.label, "Maximum players", "the schema keeps the recipe's own words");
    assert_eq!(tr.metadata.as_option().unwrap().locale, "en");

    let ja = read(&["ja", "*"]).await;
    assert_eq!(ja.locale, "en", "a language it does not speak falls back to its own");
    assert_eq!(ja.messages["config.fields.max_players.label"], "Maximum players");
    assert!(tr.messages.len() == ja.messages.len(), "no message is ever missing");
}

#[tokio::test]
async fn a_workload_s_recipe_is_read_under_its_read_action() {
    let h = Harness::new().await;
    let w = workload(&h).await;
    let (_, secret) = h.bare_token().await;
    let none =
        h.workloads(h.config(Some(secret.expose()))).get_workload_recipe(asking(&w, &[])).await;
    let e = none.unwrap_err();
    assert!(matches!(e.code, ErrorCode::NotFound | ErrorCode::PermissionDenied), "{e:?}");
    let many = vec!["en"; 33];
    let e = h.workloads(h.root()).get_workload_recipe(asking(&w, &many)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
}
