// SPDX-License-Identifier: AGPL-3.0-or-later

//! Upgrading an installation in place (§9 question 6 of docs/16-extensions.md): the same
//! installation and subject, the grants its needs still ask for kept, new needs granted from the
//! upgrader's, dropped needs revoked, and the new digest declared on the same workload.

use crate::extensions::{DNS, install, needs, push_dns, push_version};
use crate::recipes::project;
use crate::support::{Harness, detail, mutation};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

/// Version 2.0.0 of the DNS extension: it no longer needs `endpoint.read`, newly needs
/// `workload.read`, and provides a second name.
fn second() -> String {
    DNS.replace("version: \"1.0.0\"", "version: \"2.0.0\"")
        .replace("[endpoint.read]", "[workload.read]")
        .replace("to publish every endpoint of the organization", "to read what it publishes")
        .replace(
            "    - {type: dns, name: cloudflare}",
            "    - {type: dns, name: cloudflare}\n    - {type: dns, name: cloudflare-eu}",
        )
}

fn upgrade(id: &str, key: &str, version: &str) -> pb::UpgradeExtensionRequest {
    pb::UpgradeExtensionRequest {
        mutation: mutation(key).into(),
        id: id.to_owned(),
        version: version.to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn an_upgrade_keeps_the_installation_and_changes_only_what_its_needs_changed() {
    let h = Harness::new().await;
    let repo = push_dns().await;
    let project = project(&h, &h.root_org(), "dns").await;
    let approved = needs(&h, &repo, &project).await;
    let req = pb::InstallExtensionRequest {
        approved_needs: approved,
        ..install(&h, "i", &repo, &project)
    };
    let done = h.extensions(h.root()).install_extension(req).await.unwrap().into_owned();
    let before = done.extension.into_option().unwrap();
    let digest = push_version(&repo, "2.0.0", &second()).await;

    let dry = pb::UpgradeExtensionRequest { dry_run: true, ..upgrade(&before.id, "d", "2.0.0") };
    let shown = h.extensions(h.root()).upgrade_extension(dry).await.unwrap().into_owned();
    assert_eq!((shown.issued_grants.len(), shown.revoked_grants.len()), (1, 1));
    assert!(shown.plan.as_option().is_none(), "a dry run changes nothing");
    let new_needs =
        shown.extension.as_option().unwrap().manifest.as_option().unwrap().needs.clone();

    let refused = h.extensions(h.root()).upgrade_extension(upgrade(&before.id, "r", "2.0.0")).await;
    assert_eq!(detail(&refused.unwrap_err()).field, "approved_needs", "approved as installed");

    let req = pb::UpgradeExtensionRequest {
        approved_needs: new_needs,
        ..upgrade(&before.id, "u", &digest)
    };
    let done = h.extensions(h.root()).upgrade_extension(req).await.unwrap().into_owned();
    let after = done.extension.as_option().unwrap();
    assert_eq!((done.from_version.as_str(), after.version.as_str()), ("1.0.0", "2.0.0"));
    assert_eq!(
        (after.id.as_str(), after.workload.as_str()),
        (before.id.as_str(), before.workload.as_str())
    );
    assert!(done.plan.as_option().is_some(), "its workload is updated by a plan");
    let kept: Vec<_> = before.grants.iter().filter(|g| after.grants.contains(g)).collect();
    assert_eq!(kept.len(), 1, "the need it still has keeps its grant");
    assert_eq!(done.revoked_grants.len(), 1);
    assert!(!after.grants.contains(&done.revoked_grants[0]));

    let read = pb::GetExtensionRequest { id: before.id.clone(), ..Default::default() };
    let now = h.extensions(h.root()).get_extension(read).await.unwrap().into_owned().extension;
    let now = now.into_option().unwrap();
    assert_eq!(now.artifact.as_option().unwrap().digest, digest);
    assert_eq!(now.grants.len(), 2, "{:?}", now.grants);
    let listed = pb::ProviderServiceClient::new(h.http.clone(), h.root())
        .list_providers(pb::ListProvidersRequest {
            organization: h.root_org(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let names: Vec<_> = listed.health.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["cloudflare", "cloudflare-eu"]);

    let again = h.extensions(h.root()).upgrade_extension(upgrade(&before.id, "a", &digest)).await;
    let e = again.unwrap_err();
    assert_eq!((e.code, detail(&e).field.as_str()), (ErrorCode::InvalidArgument, "version"));
}
