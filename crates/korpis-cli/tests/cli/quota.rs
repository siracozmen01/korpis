// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota sets through the binary (§5.1 of docs/05-scheduling.md): the host sets its reseller's
//! set and the reseller's customer's from files, reads back what the reseller was guaranteed and
//! may sell beside what it gave below, and sets a set again from what `quota show --json` printed.

use crate::support::ControlPlane;
use korpis_proto::korpis::v1 as pb;
use serde_json::Value;

const GIB: u64 = 1 << 30;

fn ok(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A set of one entry for everything, bounding memory, overselling it by `overcommit` percent.
fn memory(reserved: u64, limit: u64, overcommit: Option<u32>) -> pb::QuotaSet {
    let entry = pb::QuotaEntry {
        reservation_bounds: pb::QuotaBounds { memory_bytes: Some(reserved), ..Default::default() }
            .into(),
        limit_bounds: pb::QuotaBounds { memory_bytes: Some(limit), ..Default::default() }.into(),
        ..Default::default()
    };
    let mut set = pb::QuotaSet { entries: vec![entry], ..Default::default() };
    if let Some(p) = overcommit {
        set.child_overcommit_percent.insert("memory_bytes".into(), p);
    }
    set
}

#[tokio::test]
async fn the_host_sets_quota_from_files_and_reads_back_what_was_given_below() {
    let cp = ControlPlane::start().await;
    let host = cp.secret.clone();
    ok(&cp.korpis(&host, &["organization", "create", "reseller"]).await);
    ok(&cp.korpis(&host, &["organization", "create", "shop", "--parent", "reseller"]).await);
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str, set: &pb::QuotaSet| {
        let path = dir.path().join(name);
        std::fs::write(&path, serde_json::to_string(set).unwrap()).unwrap();
        path.display().to_string()
    };
    let reseller = file("reseller.json", &memory(8 * GIB, 8 * GIB, Some(200)));
    let said = ok(&cp.korpis(&host, &["quota", "set", "reseller", "-f", &reseller]).await);
    assert!(said.contains("quota of reseller set"), "{said}");
    let shop = file("shop.json", &memory(2 * GIB, 16 * GIB, None));
    ok(&cp.korpis(&host, &["quota", "set", "shop", "-f", &shop]).await);

    let shown = ok(&cp.korpis(&host, &["quota", "show", "reseller"]).await);
    let (own, below) = shown.split_once("below it").unwrap();
    let row = own.lines().find(|l| l.contains("memory_bytes")).unwrap();
    let cells: Vec<&str> = row.split_whitespace().collect();
    // SHAPE DIMENSION GUARANTEED HELD GIVEN-BELOW SOLD HELD SOLD-BELOW MAY-SELL
    let (eight, two, sixteen) =
        ((8 * GIB).to_string(), (2 * GIB).to_string(), (16 * GIB).to_string());
    assert_eq!(
        cells,
        ["all", "memory_bytes", &eight, "0", &two, &eight, "0", &sixteen, &sixteen],
        "{shown}"
    );
    // ORGANIZATION SHAPE DIMENSION GUARANTEED HELD SOLD HELD: what the shop was given and holds.
    let row = below.lines().find(|l| l.contains("memory_bytes")).unwrap();
    let cells: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(cells, ["shop", "all", "memory_bytes", &two, "0", &sixteen, "0"], "{shown}");

    let printed: Value =
        serde_json::from_str(&ok(&cp.korpis(&host, &["--json", "quota", "show", "shop"]).await))
            .unwrap();
    let again = dir.path().join("again.json");
    std::fs::write(&again, printed["quota"].to_string()).unwrap();
    let again = again.display().to_string();
    ok(&cp.korpis(&host, &["quota", "set", "shop", "-f", &again]).await);
    let over = file("over.json", &memory(9 * GIB, 9 * GIB, None));
    let refused = cp.korpis(&host, &["quota", "set", "shop", "-f", &over]).await;
    assert_eq!(refused.status.code(), Some(14), "CONFLICT: more than the reseller was given");
}
