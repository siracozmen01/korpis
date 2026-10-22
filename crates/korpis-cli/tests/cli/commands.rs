// SPDX-License-Identifier: AGPL-3.0-or-later

//! The imperative commands and the listings (§2 of docs/13-surface-cli.md): names resolve
//! through what the caller can read, each command is the API's call, and `--json` prints the
//! API's own objects.

use crate::support::{ControlPlane, push_recipe};
use serde_json::Value;

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json(out: &std::process::Output) -> Value {
    serde_json::from_str(&ok(out)).unwrap()
}

/// A plan for `sleeper` saying `greeting`, proposed and waiting: its identifier.
async fn proposed(cp: &ControlPlane, repo: &str, dir: &std::path::Path, greeting: &str) -> String {
    let path = dir.join("sleeper.yaml");
    let text = format!(
        "workload: sleeper\nproject: games\nrecipe: {repo}:1.0.0\nconfig:\n  greeting: {greeting}\n"
    );
    std::fs::write(&path, text).unwrap();
    let planned =
        json(&cp.korpis(&cp.secret, &["--json", "plan", "-f", path.to_str().unwrap()]).await);
    planned[0]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn the_commands_are_the_api_by_name() {
    let cp = ControlPlane::start().await;
    cp.project("games").await;
    let repo = push_recipe().await;
    let dir = tempfile::tempdir().unwrap();
    let s = cp.secret.clone();

    let orgs = ok(&cp.korpis(&s, &["get", "organizations"]).await);
    assert!(orgs.starts_with("ID") && orgs.contains("acme"), "{orgs}");
    assert!(ok(&cp.korpis(&s, &["get", "projects"]).await).contains("games"));

    let first = proposed(&cp, &repo, dir.path(), "hello").await;
    let approved = json(&cp.korpis(&s, &["--json", "plan", "approve", &first]).await);
    assert_eq!(approved["status"], "PLAN_STATUS_APPROVED", "{approved}");
    let second = proposed(&cp, &repo, dir.path(), "hi").await;
    let rejected =
        json(&cp.korpis(&s, &["--json", "plan", "reject", &second, "--reason", "no"]).await);
    assert_eq!(rejected["status"], "PLAN_STATUS_REJECTED", "{rejected}");

    let listed = ok(&cp.korpis(&s, &["get", "workloads", "games"]).await);
    assert!(listed.contains("sleeper"), "{listed}");
    let plans = json(&cp.korpis(&s, &["--json", "get", "plans", "games/sleeper"]).await);
    assert_eq!(plans.as_array().unwrap().len(), 2, "{plans}");
    let status = ok(&cp.korpis(&s, &["status", "sleeper"]).await);
    assert!(status.starts_with("sleeper (wl_"), "{status}");

    let stopped = json(&cp.korpis(&s, &["--json", "workload", "stop", "sleeper"]).await);
    assert_eq!(stopped["power"]["desired"], "POWER_STATE_STOPPED", "{stopped}");
    let restarted = ok(&cp.korpis(&s, &["workload", "restart", "games/sleeper"]).await);
    assert!(restarted.contains("cycle 1"), "{restarted}");

    let quiet = ["workload", "classify", "sleeper", "stop=routine", "--reason", "drained first"];
    let classified = ok(&cp.korpis(&s, &quiet).await);
    assert!(classified.contains("stop") && classified.contains("routine"), "{classified}");
    let bad =
        cp.korpis(&s, &["workload", "classify", "sleeper", "stop=quiet", "--reason", "x"]).await;
    assert_eq!(bad.status.code(), Some(1), "a class is disruptive or routine");

    let effects = ok(&cp.korpis(&s, &["get", "effects", "--workload", "sleeper"]).await);
    assert!(effects.lines().count() > 2 && effects.contains("grt_"), "named grants: {effects}");
    assert!(effects.contains("RELAYED BY"), "the relay has a column: {effects}");
    // Naming nothing reads the caller's own organization's subtree, here by action.
    let ours =
        ok(&cp.korpis(&s, &["get", "effects", "--action", "project.", "--limit", "5"]).await);
    assert!(ours.contains("project.create") && ours.contains("prj_"), "about its project: {ours}");
    assert!(!ours.contains("workload.restart"), "only what the action names: {ours}");

    let token = cp
        .korpis(&s, &["node", "token", "create", "n1", "--label", "region=eu", "--expires", "30m"])
        .await;
    let token = ok(&token);
    assert!(token.contains("korpis-agent join --server https://localhost:"), "{token}");
    assert!(token.contains("--ca-fingerprint sha256:"), "{token}");
    let ghost = cp.korpis(&s, &["node", "cordon", "ghost"]).await;
    assert_eq!(ghost.status.code(), Some(1), "a name nobody can read is a local error");
    let unconfirmed = cp.korpis(&s, &["node", "remove", "ghost"]).await;
    assert_eq!(unconfirmed.status.code(), Some(1), "removing a node is said with --yes");
    let said = String::from_utf8_lossy(&unconfirmed.stderr).into_owned();
    assert!(said.contains("--yes"), "{said}");

    let retired = json(&cp.korpis(&s, &["--json", "workload", "delete", "sleeper"]).await);
    assert!(retired["id"].as_str().unwrap().starts_with("pln_"), "{retired}");
}

#[tokio::test]
async fn a_domain_is_asked_for_with_the_record_to_publish_and_attested_at_once() {
    let cp = ControlPlane::start().await;
    let s = cp.secret.clone();
    let asked = ok(&cp.korpis(&s, &["domain", "create", "shop.example.com"]).await);
    assert!(asked.contains("pending"), "{asked}");
    assert!(asked.contains("_korpis-verify.shop.example.com  TXT  \""), "{asked}");
    assert!(asked.contains("korpis domain verify dom_"), "{asked}");

    let attested =
        json(&cp.korpis(&s, &["--json", "domain", "create", "example.org", "--attest"]).await);
    assert_eq!(attested["verification"], "DOMAIN_VERIFICATION_OPERATOR", "{attested}");
    assert!(attested["verifiedAt"].is_string(), "verified at once: {attested}");
    let id = attested["id"].as_str().unwrap();
    let listed = ok(&cp.korpis(&s, &["domain", "list"]).await);
    assert!(listed.contains("example.org") && listed.contains("operator"), "{listed}");
    let gone = ok(&cp.korpis(&s, &["domain", "delete", id]).await);
    assert_eq!(gone, "deleted 1 domain\n");
    let bad = cp.korpis(&s, &["domain", "create", "*.example.org"]).await;
    assert_eq!(bad.status.code(), Some(10), "invalid: no wildcard is held");
}

#[tokio::test]
async fn a_certificate_is_uploaded_from_its_files_and_shown_without_its_key() {
    let cp = ControlPlane::start().await;
    let s = cp.secret.clone();
    let dir = tempfile::tempdir().unwrap();
    let made = rcgen::generate_simple_self_signed(vec!["shop.example.com".into()]).unwrap();
    let (chain, key) = (dir.path().join("chain.pem"), dir.path().join("key.pem"));
    std::fs::write(&chain, made.cert.pem()).unwrap();
    std::fs::write(&key, made.signing_key.serialize_pem()).unwrap();
    let (chain, key) = (chain.to_str().unwrap(), key.to_str().unwrap());
    let args = ["--json", "certificate", "create", "--chain", chain, "--key", key];
    let uploaded = json(&cp.korpis(&s, &args).await);
    assert_eq!(uploaded["names"][0], "shop.example.com", "{uploaded}");
    assert_eq!(uploaded["state"], "CERTIFICATE_STATE_ISSUED", "{uploaded}");
    let id = uploaded["id"].as_str().unwrap();
    let listed = ok(&cp.korpis(&s, &["certificate", "list"]).await);
    assert!(listed.contains(id) && listed.contains("provided"), "{listed}");
    let shown = ok(&cp.korpis(&s, &["certificate", "get", id]).await);
    assert!(shown.contains("BEGIN CERTIFICATE") && !shown.contains("PRIVATE"), "{shown}");
    assert_eq!(ok(&cp.korpis(&s, &["certificate", "delete", id]).await), "deleted\n");
    let missing = cp.korpis(&s, &["certificate", "create", "--chain", "/none", "--key", key]).await;
    assert_ne!(missing.status.code(), Some(0), "a file that is not there");
}

#[tokio::test]
async fn a_theme_is_set_from_files_and_flags_shown_and_cleared() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (logo, css) = (dir.path().join("logo.svg"), dir.path().join("brand.css"));
    std::fs::write(&logo, "<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
    std::fs::write(&css, ".brand { font-weight: 600 }").unwrap();
    let s = cp.secret.clone();
    let set = json(
        &cp.korpis(
            &s,
            &[
                "--json",
                "theme",
                "set",
                "--product-name",
                "Acme Hosting",
                "--logo",
                logo.to_str().unwrap(),
                "--stylesheet",
                css.to_str().unwrap(),
                "--light",
                "accent=#1F5FBF",
            ],
        )
        .await,
    );
    assert_eq!(set["theme"]["productName"], "Acme Hosting", "{set}");
    assert_eq!(set["theme"]["light"]["accent"], "#1f5fbf", "{set}");
    assert_eq!(set["ownVersion"], "1", "{set}");
    let shown = ok(&cp.korpis(&s, &["theme", "show"]).await);
    assert!(shown.contains("Acme Hosting") && shown.contains("image/svg+xml"), "{shown}");
    let bad = cp.korpis(&s, &["theme", "set", "--dark", "text=red"]).await;
    assert!(!bad.status.success() && String::from_utf8_lossy(&bad.stderr).contains("dark.text"));
    let cleared = ok(&cp.korpis(&s, &["theme", "clear"]).await);
    assert!(cleared.contains("the panel's own theme"), "{cleared}");
}

#[tokio::test]
async fn a_layout_is_set_from_a_file_shown_set_again_as_shown_and_cleared() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("layout.json");
    let notice =
        serde_json::json!({"notice": {"heading": "Maintenance tonight", "text": "At 22:00."}});
    let layout = serde_json::json!({"name": "Acme", "items": [notice, {"places": {}}]});
    std::fs::write(&file, layout.to_string()).unwrap();
    let (s, from) = (cp.secret.clone(), file.to_str().unwrap());
    let set = json(&cp.korpis(&s, &["--json", "layout", "set", "--from", from]).await);
    assert_eq!(set["layout"]["name"], "Acme", "{set}");
    assert_eq!(set["ownVersion"], "1", "{set}");

    // What `show` prints is what `set` takes.
    let shown = json(&cp.korpis(&s, &["--json", "layout", "show"]).await);
    std::fs::write(&file, shown["layout"].to_string()).unwrap();
    let again = json(&cp.korpis(&s, &["--json", "layout", "set", "--from", from]).await);
    assert_eq!((&again["layout"], &again["ownVersion"]), (&shown["layout"], &"2".into()));
    let human = ok(&cp.korpis(&s, &["layout", "show"]).await);
    assert!(human.contains("a notice: Maintenance tonight"), "{human}");

    let link = serde_json::json!({"link": {"label": "Help", "url": "http://help.example"}});
    let bad = serde_json::json!({"items": [{"places": {}}, link]});
    std::fs::write(&file, bad.to_string()).unwrap();
    let refused = cp.korpis(&s, &["layout", "set", "--from", from]).await;
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success() && said.contains("items[1].link.url"), "{said}");
    let cleared = ok(&cp.korpis(&s, &["layout", "clear"]).await);
    assert!(cleared.contains("the panel's own landing"), "{cleared}");
}

#[tokio::test]
async fn a_token_holds_no_ssh_keys_and_is_told_so() {
    let cp = ControlPlane::start().await;
    let s = cp.secret.clone();
    let listed = cp.korpis(&s, &["ssh-key", "list"]).await;
    let said = String::from_utf8_lossy(&listed.stderr);
    assert!(!listed.status.success() && said.contains("only a signed-in user"), "{said}");
}
