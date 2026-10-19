// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tenancy tree through the binary (§2 of docs/08-identity.md): the host makes a reseller's
//! organization and gives the reseller authority over it, and the reseller, holding nothing
//! above its own organization, makes its customer beneath it and a project in the customer.

use crate::support::ControlPlane;
use serde_json::Value;

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn a_reseller_makes_its_customer_and_a_project_beneath_its_own_organization() {
    let cp = ControlPlane::start().await;
    let host = cp.secret.clone();
    let made: Value = serde_json::from_str(&ok(&cp
        .korpis(&host, &["--json", "organization", "create", "reseller"])
        .await))
    .unwrap();
    assert_eq!(made["parent"], cp.root_org.as_str(), "beneath the host's own");
    let admin = "org.create,org.read,project.create,project.read";
    let issue = [
        "--json",
        "grant",
        "issue",
        "--actions",
        admin,
        "--scope",
        "org:reseller",
        "--delegate",
        admin,
        "--depth",
        "2",
        "--label",
        "reseller",
    ];
    let issued: Value = serde_json::from_str(&ok(&cp.korpis(&host, &issue).await)).unwrap();
    let reseller = issued["token"].as_str().unwrap().to_owned();

    // Its own organization is the top of what it can read, so it is where `create` puts things.
    let customer = ok(&cp.korpis(&reseller, &["organization", "create", "customer"]).await);
    assert!(customer.contains("customer") && customer.contains("parent:"), "{customer}");
    let shop = ok(&cp
        .korpis(&reseller, &["project", "create", "shop", "--organization", "customer"])
        .await);
    assert!(shop.contains("project shop"), "{shop}");
    let shown = ok(&cp.korpis(&reseller, &["organization", "show", "customer"]).await);
    assert!(shown.contains("state:   active"), "{shown}");

    // Nothing above its own: the host's organization is not a name to it.
    let above =
        cp.korpis(&reseller, &["organization", "create", "beside", "--parent", &cp.root_org]).await;
    assert_eq!(above.status.code(), Some(13), "NOT_FOUND: the host's is not there for it");
    let listed = ok(&cp.korpis(&reseller, &["get", "organizations"]).await);
    assert!(listed.contains("reseller") && listed.contains("customer"), "{listed}");
}
