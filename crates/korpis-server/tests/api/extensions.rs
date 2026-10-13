// SPDX-License-Identifier: AGPL-3.0-or-later

//! Extensions through the API (docs/16-extensions.md): an extension's artifact is installed,
//! never declared as a workload of its own; installing it declares its workload and issues it one
//! grant per approved need, each attenuated from one its installer holds.

use crate::recipes::{body, change, create, project, push_paper, registry_address};
use crate::support::{Harness, detail, mutation, reason, test_key};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use korpis_recipe::artifact::Artifact;
use korpis_recipe::registry::Registry;
use uuid::Uuid;

/// The DNS extension the recipe crate's tests load (§3.1 of docs/16-extensions.md).
pub const DNS: &str = include_str!("../../../korpis-recipe/tests/recipe/dns.yaml");

/// Pushes the DNS extension, signed by the signer every harness's root organization accepts;
/// returns the repository.
pub async fn push_dns() -> String {
    push_document(DNS).await
}

/// Pushes an extension document, signed as [`push_dns`] signs; returns the repository.
pub async fn push_document(document: &str) -> String {
    let repo = format!("{}/test/dns-{}", registry_address(), Uuid::now_v7().simple());
    push_version(&repo, "1.0.0", document).await;
    repo
}

/// Pushes `document` to `repo` as `tag`, signed; returns its digest.
pub async fn push_version(repo: &str, tag: &str, document: &str) -> String {
    let registry = Registry::new(vec![registry_address()]);
    let artifact = Artifact { document: document.as_bytes().to_vec(), files: Vec::new() };
    let digest = registry.push(repo, tag, &artifact).await.unwrap();
    registry.sign(repo, &digest, &test_key()).await.unwrap();
    digest
}

#[tokio::test]
async fn an_extension_is_installed_and_never_declared_as_a_plain_workload() {
    let h = Harness::new().await;
    let repo = push_dns().await;
    let project = project(&h, &h.root_org(), "dns").await;
    let mut declared = body(&repo, &[]);
    if let Some(r) = declared.recipe.as_option_mut() {
        r.version = "1.0.0".into();
    }
    let e = h
        .workloads(h.root())
        .declare_intent(create("plain", &project, "dns", declared))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    let d = detail(&e);
    assert_eq!(d.field, "recipe");
    assert!(d.message.contains("installed, not declared"), "{}", d.message);
}

/// An installation request for the DNS extension into the root organization, its workload in
/// `project`.
pub fn install(h: &Harness, key: &str, repo: &str, project: &str) -> pb::InstallExtensionRequest {
    pb::InstallExtensionRequest {
        mutation: mutation(key).into(),
        artifact: repo.to_owned(),
        version: "1.0.0".into(),
        organization: h.root_org(),
        workload_project: project.to_owned(),
        ..Default::default()
    }
}

/// The needs a dry run shows the installer.
pub async fn needs(h: &Harness, repo: &str, project: &str) -> Vec<pb::Need> {
    let dry = pb::InstallExtensionRequest { dry_run: true, ..install(h, "dry", repo, project) };
    let shown = h.extensions(h.root()).install_extension(dry).await.unwrap().into_owned();
    let extension = shown.extension.into_option().unwrap();
    assert!(shown.plan.as_option().is_none(), "a dry run declares nothing");
    extension.manifest.into_option().unwrap().needs
}

#[tokio::test]
async fn an_installation_runs_as_its_own_subject_under_grants_its_installer_could_give() {
    let h = Harness::new().await;
    let repo = push_dns().await;
    let project = project(&h, &h.root_org(), "dns").await;
    let needs = needs(&h, &repo, &project).await;
    assert_eq!(needs.len(), 2);

    let req = pb::InstallExtensionRequest {
        approved_needs: needs.clone(),
        ..install(&h, "i", &repo, &project)
    };
    let done = h.extensions(h.root()).install_extension(req).await.unwrap().into_owned();
    let installed = done.extension.into_option().unwrap();
    assert_eq!((installed.name.as_str(), installed.version.as_str()), ("cloudflare-dns", "1.0.0"));
    assert_eq!(installed.grants.len(), 2, "one grant per approved need");
    assert!(done.plan.as_option().is_some(), "its workload is declared");

    let read = pb::GetExtensionRequest { id: installed.id.clone(), ..Default::default() };
    let got = h.extensions(h.root()).get_extension(read).await.unwrap().into_owned();
    let got = got.extension.into_option().unwrap();
    assert_eq!(got.grants.len(), 2);
    assert_eq!(got.workload, installed.workload);
    assert!(got.artifact.as_option().is_some_and(|a| a.digest.starts_with("sha256:")));
    let list = pb::ListExtensionsRequest { organization: h.root_org(), ..Default::default() };
    let listed = h.extensions(h.root()).list_extensions(list).await.unwrap().into_owned();
    assert_eq!(listed.extensions.len(), 1);
    let issued = h.effects("grant.issued").await.len();
    assert!(issued >= 2 && h.effects("extension.install").await.len() == 1);

    // The same name again in the same place is refused, and so is a plain recipe on its workload.
    let again =
        pb::InstallExtensionRequest { approved_needs: needs, ..install(&h, "i2", &repo, &project) };
    let e = h.extensions(h.root()).install_extension(again).await.unwrap_err();
    assert_eq!(reason(&e), "extension.exists");
    let paper = push_paper().await;
    let swap = change("swap", &installed.workload, 1, body(&paper, &[]));
    let e = h.workloads(h.root()).declare_intent(swap).await.unwrap_err();
    assert!(detail(&e).message.contains("installed from"), "{}", detail(&e).message);
}

#[tokio::test]
async fn needs_are_approved_exactly_and_only_from_grants_the_installer_can_give() {
    let h = Harness::new().await;
    let repo = push_dns().await;
    let project = project(&h, &h.root_org(), "dns").await;
    let mut needs = needs(&h, &repo, &project).await;

    let fewer = pb::InstallExtensionRequest {
        approved_needs: needs[..1].to_vec(),
        ..install(&h, "fewer", &repo, &project)
    };
    let e = h.extensions(h.root()).install_extension(fewer).await.unwrap_err();
    assert_eq!(detail(&e).field, "approved_needs", "nothing granted the installer did not read");

    // A token holding every action needed, but nothing it may pass on, can give nothing.
    let (token, secret) = h.bare_token().await;
    let actions = ["extension.install", "workload.create", "endpoint.read", "event.read"];
    h.issue_to(token, "t", &actions).await;
    let req = pb::InstallExtensionRequest {
        approved_needs: needs.clone(),
        ..install(&h, "token", &repo, &project)
    };
    let e = h.extensions(h.config(Some(secret.expose()))).install_extension(req).await.unwrap_err();
    assert_eq!(detail(&e).field, "approved_needs[0]");

    needs.swap(0, 1);
    let reordered =
        pb::InstallExtensionRequest { approved_needs: needs, ..install(&h, "o", &repo, &project) };
    let e = h.extensions(h.root()).install_extension(reordered).await.unwrap_err();
    assert_eq!(detail(&e).field, "approved_needs", "the list as the document gives it");
}

#[tokio::test]
async fn uninstalling_revokes_its_grants_proposes_its_retirement_and_frees_its_name() {
    let h = Harness::new().await;
    let repo = push_dns().await;
    let project = project(&h, &h.root_org(), "dns").await;
    let needs = needs(&h, &repo, &project).await;
    let req = |key: &str| pb::InstallExtensionRequest {
        approved_needs: needs.clone(),
        ..install(&h, key, &repo, &project)
    };
    let done = h.extensions(h.root()).install_extension(req("i1")).await.unwrap().into_owned();
    let first = done.extension.into_option().unwrap();

    let uninstall = |key: &str| pb::UninstallExtensionRequest {
        mutation: mutation(key).into(),
        id: first.id.clone(),
        ..Default::default()
    };
    let gone = h.extensions(h.root()).uninstall_extension(uninstall("u1")).await.unwrap();
    let plan = gone.into_owned().plan.into_option().expect("its workload's retirement");
    assert_eq!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED));
    assert_eq!(h.effects("grant.revoke").await.len(), 2, "both of its grants");
    let e = h.extensions(h.root()).uninstall_extension(uninstall("u2")).await.unwrap_err();
    assert_eq!(reason(&e), "extension.uninstalled");
    let list = pb::ListExtensionsRequest { organization: h.root_org(), ..Default::default() };
    let listed = h.extensions(h.root()).list_extensions(list).await.unwrap().into_owned();
    assert!(listed.extensions.is_empty(), "an uninstalled extension is not listed");

    // Its name is free again where it was installed; its workload's name is not, until retired.
    let again = pb::InstallExtensionRequest { workload_name: "dns-2".into(), ..req("i2") };
    let second = h.extensions(h.root()).install_extension(again).await.unwrap().into_owned();
    assert_ne!(second.extension.into_option().unwrap().id, first.id, "a new subject");
}

#[tokio::test]
async fn an_extension_this_control_plane_cannot_serve_is_refused_at_install_naming_why() {
    let h = Harness::new().await;
    let project = project(&h, &h.root_org(), "dns").await;
    let requires = "requires: [provider.serve, events.subscribe]";
    let wants_dns = push_document(&DNS.replace(requires, "requires: [provider.dns]")).await;
    let dry = |repo: &str| pb::InstallExtensionRequest {
        dry_run: true,
        ..install(&h, "dry", repo, &project)
    };
    let e = h.extensions(h.root()).install_extension(dry(&wants_dns)).await.unwrap_err();
    assert_eq!(reason(&e), "extension.incompatible");
    assert!(e.message.as_deref().unwrap_or_default().contains("provider.dns"), "{e:?}");

    let later = DNS.replace("api_version: korpis.extension.v1", "api_version: korpis.extension.v2");
    let e = h.extensions(h.root()).install_extension(dry(&push_document(&later).await)).await;
    let e = e.unwrap_err();
    assert_eq!(reason(&e), "extension.incompatible");
    assert!(e.message.as_deref().unwrap_or_default().contains("korpis.extension.v2"), "{e:?}");

    let info = pb::ServerInfoServiceClient::new(h.http.clone(), h.config(None))
        .get_server_info(pb::GetServerInfoRequest::default())
        .await
        .unwrap()
        .into_owned();
    assert_eq!(info.extension_contracts, ["korpis.extension.v1"]);
    assert!(info.capabilities.iter().any(|c| c == "provider.serve"), "{:?}", info.capabilities);
}
