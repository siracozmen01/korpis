// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis get usage` and `korpis get periods` (§2 of docs/15-observability.md): each figure in
//! its unit with the window it covers and how it was measured, an absent one never a zero, and
//! the API's records under `--json`.

use crate::support::{ControlPlane, push_recipe};
use korpis_core::id::{Id, IdKind};
use korpis_core::metering::{Counters, Record, RecordKind, day_of};
use serde_json::Value;
use std::time::{Duration, SystemTime};

const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn rfc3339(t: SystemTime) -> String {
    jiff::Timestamp::try_from(t).unwrap().to_string()
}

fn record(workload: uuid::Uuid, from: SystemTime, kind: RecordKind) -> Record {
    Record {
        id: uuid::Uuid::now_v7(),
        workload,
        node: None,
        lease_epoch: 1,
        from,
        to: from + HOUR,
        kind,
        cause: (kind == RecordKind::Gap).then(|| "node unreachable".to_owned()),
        supersedes: Vec::new(),
        used: Counters::default(),
        memory_peak_bytes: None,
        disk_bytes: None,
        losses: Vec::new(),
        recorded_at: None,
    }
}

/// The sleeper workload of project `games`, applied, as its identifier.
async fn sleeper(cp: &ControlPlane) -> String {
    cp.project("games").await;
    let repo = push_recipe().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sleeper.yaml");
    let text = format!(
        "workload: sleeper\nproject: games\nrecipe: {repo}:1.0.0\nconfig:\n  greeting: hi\n"
    );
    std::fs::write(&file, text).unwrap();
    ok(&cp.korpis(&cp.secret, &["apply", "-f", file.to_str().unwrap()]).await);
    let listed = ok(&cp.korpis(&cp.secret, &["--json", "get", "workloads", "games"]).await);
    let listed: Value = serde_json::from_str(&listed).unwrap();
    listed[0]["workload"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn usage_is_shown_in_its_units_with_its_window_and_the_days_with_their_state() {
    let cp = ControlPlane::start().await;
    let workload = sleeper(&cp).await;
    let id = Id::parse(IdKind::Workload, &workload).unwrap().uuid();
    let now = SystemTime::now();
    let day = day_of(now) - DAY * 3;
    let mut measured = record(id, day + HOUR, RecordKind::Measured);
    measured.used.cpu_usec = Some(1_500_000);
    measured.used.memory_kib_seconds = Some(1024 * 1024 * 3600 * 2);
    measured.disk_bytes = Some(0);
    cp.db.record_metering(&measured, now).await.unwrap();
    cp.db.record_metering(&record(id, day + HOUR * 2, RecordKind::Gap), now).await.unwrap();

    let (from, to) = (rfc3339(day), rfc3339(day + DAY));
    let window = ["--from", from.as_str(), "--to", to.as_str()];
    let s = &cp.secret;
    let mut args = vec!["get", "usage", "--workload", "games/sleeper"];
    args.extend(window);
    let shown = ok(&cp.korpis(s, &args).await);
    assert!(shown.contains(&rfc3339(day + HOUR)) && shown.contains(&rfc3339(day + HOUR * 2)));
    assert!(shown.contains("1.500 s") && shown.contains("2.000 GiB-h"), "{shown}");
    assert!(shown.contains("0 B"), "a disk measured empty is a zero: {shown}");
    assert!(shown.contains("gap") && shown.contains("node unreachable"), "{shown}");
    assert!(shown.contains("differenced between two samples"), "how it was measured: {shown}");

    // The organization's subtree by default, page by page under --limit.
    let mut args = vec!["--json", "get", "usage", "--limit", "1"];
    args.extend(window);
    let json: Value = serde_json::from_str(&ok(&cp.korpis(s, &args).await)).unwrap();
    let records = json.as_array().unwrap();
    assert_eq!(records.len(), 1, "{json}");
    assert_eq!(records[0]["workload"], workload.as_str());
    assert_eq!(records[0]["cpuUsec"], "1500000");
    assert!(records[0].get("cpuThrottledUsec").is_none(), "absent, never a zero: {json}");
    let mut args = vec!["--json", "get", "usage", "--org", cp.root_org.as_str()];
    args.extend(window);
    let json: Value = serde_json::from_str(&ok(&cp.korpis(s, &args).await)).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 2, "{json}");
    assert_eq!(json[1]["kind"], "USAGE_KIND_GAP");

    let to = rfc3339(day + DAY * 2);
    let shown = ok(&cp.korpis(s, &["get", "periods", "--from", &from, "--to", &to]).await);
    assert_eq!(shown.lines().count(), 3, "a header and two days: {shown}");
    assert!(!shown.contains("open"), "both days ended over a day ago: {shown}");
    let bad = cp.korpis(s, &["get", "usage", "--from", "yesterday"]).await;
    assert!(!bad.status.success(), "a time that is none of the three forms");
    assert!(String::from_utf8_lossy(&bad.stderr).contains("RFC 3339"));
}
