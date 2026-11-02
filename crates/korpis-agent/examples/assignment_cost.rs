// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a change to one workload costs a node holding many: the bytes sent, and the time the agent
//! takes to believe and keep what it is told, for the whole set and for a delta. Each assignment is
//! padded to the size given, since what one holds varies with its recipe and intent.
//!
//! `cargo run --release -p korpis-agent --example assignment_cost -- [bytes per assignment]`

use buffa::Message as _;
use ed25519_dalek::{Signer as _, SigningKey};
use korpis_agent::held::HeldSet;
use korpis_core::id::IdKind;
use korpis_core::workload::assignment::set_digest;
use korpis_proto::korpis::agent::v1 as agent;
use std::error::Error;
use std::io::Write as _;
use std::time::{Duration, Instant};
use uuid::Uuid;

const NODE: &str = "nd_measured";

fn signed(
    key: &SigningKey,
    workload: Uuid,
    generation: u64,
    pad: usize,
) -> agent::SignedAssignment {
    let a = agent::Assignment {
        workload: korpis_wire::wire_id(IdKind::Workload, workload),
        node: NODE.to_owned(),
        generation,
        recipe_manifest: vec![0x5a; pad],
        ..Default::default()
    };
    let body = a.encode_to_vec();
    let signature = key.sign(&body).to_bytes().to_vec();
    agent::SignedAssignment {
        assignment: body,
        signature,
        key_id: "k".into(),
        ..Default::default()
    }
}

/// The time `f` takes, the least of five runs.
fn least<T>(mut f: impl FnMut() -> Result<T, Box<dyn Error>>) -> Result<Duration, Box<dyn Error>> {
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let at = Instant::now();
        f()?;
        best = best.min(at.elapsed());
    }
    Ok(best)
}

fn measure(n: usize, pad: usize, dir: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let key = SigningKey::from_bytes(&[7; 32]);
    let public = key.verifying_key().to_bytes().to_vec();
    let keys = [agent::SigningKey { key_id: "k".into(), public_key: public, ..Default::default() }];
    let mut entries: Vec<(Uuid, u64)> = (0..n).map(|_| (Uuid::now_v7(), 1)).collect();
    let set = agent::AssignmentSet {
        assignments: entries.iter().map(|&(w, g)| signed(&key, w, g, pad)).collect(),
        set_digest: set_digest(&entries),
        ..Default::default()
    };
    let held = HeldSet::verify(&set, &keys, NODE)?;
    let changed = entries.first_mut().ok_or("no workloads")?;
    changed.1 = 2;
    let upsert = signed(&key, changed.0, 2, pad);
    let delta = agent::AssignmentDelta {
        base_digest: held.digest.clone(),
        upserts: vec![upsert],
        set_digest: set_digest(&entries),
        ..Default::default()
    };
    let path = dir.join(format!("set-{n}.binpb"));
    let whole = least(|| Ok(HeldSet::verify(&set, &keys, NODE)?))?;
    let applied = least(|| Ok(held.apply(&delta, &keys, NODE).ok_or("not its base")??))?;
    let saved = least(|| Ok(held.save(&path)?))?;
    writeln!(
        std::io::stdout().lock(),
        "{n:>6} {:>12} {:>8} {:>12.2?} {:>10.2?} {:>10.2?}",
        set.encode_to_vec().len(),
        delta.encode_to_vec().len(),
        whole,
        applied,
        saved
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let pad = std::env::args().nth(1).map(|a| a.parse()).transpose()?.unwrap_or(2048);
    let dir = std::env::temp_dir().join(format!("korpis-assignment-cost-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "assignments of {pad} bytes")?;
    let (held, set, delta, verify, apply, keep) =
        ("held", "set bytes", "delta", "verify set", "apply", "keep");
    writeln!(out, "{held:>6} {set:>12} {delta:>8} {verify:>12} {apply:>10} {keep:>10}")?;
    drop(out);
    for n in [10, 100, 1000, 5000] {
        measure(n, pad, &dir)?;
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
