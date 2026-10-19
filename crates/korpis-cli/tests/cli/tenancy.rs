// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tenancy tree through the binary (§2 of docs/08-identity.md): the host makes a reseller's
//! organization and gives the reseller authority over it, and the reseller, holding nothing
//! above its own organization, makes its customer beneath it and a project in the customer. The
//! host suspends the reseller from above and resumes it, and the reseller, suspended, is shown why
//! and what it still reads.

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

#[tokio::test]
async fn the_host_suspends_a_customer_who_is_shown_why_and_resumes_it() {
    let cp = ControlPlane::start().await;
    let host = cp.secret.clone();
    ok(&cp.korpis(&host, &["organization", "create", "customer"]).await);
    ok(&cp.korpis(&host, &["organization", "create", "shop", "--parent", "customer"]).await);
    let beneath = ok(&cp.korpis(&host, &["get", "organizations", "--parent", "customer"]).await);
    assert!(beneath.contains("shop") && !beneath.contains(" customer "), "{beneath}");

    let why = "the invoice of October is unpaid";
    let suspend = ["organization", "suspend", "customer", "--reason", why, "--stop"];
    let shown = ok(&cp.korpis(&host, &suspend).await);
    assert!(shown.contains("state:   suspended") && shown.contains(why), "{shown}");
    assert!(shown.contains("workloads: held stopped"), "{shown}");
    assert!(shown.contains("stay readable"), "and what it still reads: {shown}");
    let listed = ok(&cp.korpis(&host, &["get", "organizations"]).await);
    let row = listed.lines().find(|l| l.contains("customer")).unwrap();
    assert!(row.ends_with("suspended"), "{listed}");
    let again = cp.korpis(&host, &suspend).await;
    assert_eq!(again.status.code(), Some(14), "CONFLICT: it is suspended already");

    let resumed = ok(&cp.korpis(&host, &["--json", "organization", "resume", "customer"]).await);
    let resumed: Value = serde_json::from_str(&resumed).unwrap();
    assert_eq!(resumed["status"], "ORGANIZATION_STATUS_ACTIVE", "{resumed}");
    assert!(resumed.get("suspension").is_none_or(Value::is_null), "{resumed}");
}
