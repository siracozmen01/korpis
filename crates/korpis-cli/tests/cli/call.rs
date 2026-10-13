// SPDX-License-Identifier: AGPL-3.0-or-later

//! Every API operation has a command; the answer is the API's own JSON; the exit code is the
//! error's code (§1 and §2 of docs/13-surface-cli.md).

use crate::support::ControlPlane;
use serde_json::Value;

fn json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[tokio::test]
async fn the_cli_calls_the_api_and_exits_with_its_codes() {
    let cp = ControlPlane::start().await;
    let s = cp.secret.clone();

    let v = cp.korpis(&s, &["version", "--json"]).await;
    let v = json(&v);
    assert_eq!(v["client"]["sourceUrl"], v["server"]["sourceUrl"], "{v}");
    assert!(v["server"]["sourceRevision"].is_string());

    let list = cp.korpis(&s, &["call", "--list"]).await;
    assert_eq!(String::from_utf8_lossy(&list.stdout).lines().count(), 37);

    // A mutation needs no hand-made idempotency key.
    let data = format!(r#"{{"organization": "{}", "name": "games"}}"#, cp.root_org);
    let made = cp.korpis(&s, &["call", "ProjectService/CreateProject", "-d", &data]).await;
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    assert_eq!(json(&made)["project"]["name"], "games", "the API's own shape");

    let missing = r#"{"id": "wl_00000000000000000000000000000000"}"#;
    let nf = cp.korpis(&s, &["--json", "call", "WorkloadService/GetWorkload", "-d", missing]).await;
    assert_eq!(nf.status.code(), Some(13), "NOT_FOUND");
    let err: Value = serde_json::from_slice(&nf.stderr).unwrap();
    assert_eq!(err["code"], "NOT_FOUND");

    let anonymous = cp.korpis("", &["call", "ProjectService/ListProjects"]).await;
    assert_eq!(anonymous.status.code(), Some(11), "UNAUTHENTICATED");
    let local = cp.korpis(&s, &["call", "WorkloadService/GetWorkload", "-d", "{not json"]).await;
    assert_eq!(local.status.code(), Some(1), "found before anything was sent");
}
