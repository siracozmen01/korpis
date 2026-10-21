// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bandwidth, shaped (§7 of docs/07-networking.md), written with iproute2's `tc` as the rules are
//! written with `nft`. Every qdisc Korpis makes has the handle `4b50:`, so a tree of its own is
//! told apart from another program's and found again without a ledger. An uplink's tree is a root
//! class of the uplink's capacity, a default class whose floor is what the node keeps for itself,
//! and a class for each workload that reserves, its rate the reservation and its ceiling the
//! limit. A workload's link holds its limit on everything it sends, on `eth0` inside its
//! namespace, and its ingress limit on the host's side, each one class. Each leaf queues by
//! `fq_codel`, so one flow's backlog does not delay the others behind it.

use std::io;
use std::process::Command;

/// The major number of every qdisc Korpis makes, in hexadecimal as `tc` writes handles.
pub(crate) const MAJOR: u32 = 0x4b50;
/// The class of an uplink's whole capacity.
const ROOT: u32 = 1;
/// The class of the node's own traffic and of whatever reserves nothing.
const DEFAULT: u32 = 2;
/// The first class a workload is given.
pub(crate) const FIRST: u32 = 0x10;
/// The bytes each class may send in its turn: some forty full frames, where HTB's own, its rate
/// over ten, is megabytes at gigabit rates and lets one class send a burst that long.
const QUANTUM: &str = "60000";

/// The handle of class `minor`, as `tc` and nftables write it: `4b50:10`.
pub(crate) fn id(minor: u32) -> String {
    format!("{MAJOR:x}:{minor:x}")
}

fn qdisc_handle() -> String {
    format!("{MAJOR:x}:")
}

fn rate(bits: u64) -> String {
    format!("{}bit", bits.max(8))
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// An uplink: its interface, its capacity, and what the node keeps of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Uplink {
    pub(crate) interface: String,
    pub(crate) capacity: u64,
    pub(crate) reserve: u64,
}

/// The commands making `u`'s tree, `present` when its root qdisc is Korpis's already, so the
/// workloads' classes beneath it are kept.
pub(crate) fn uplink(u: &Uplink, present: bool) -> Vec<Vec<String>> {
    let (dev, cap) = (u.interface.as_str(), rate(u.capacity));
    let reserve = rate(u.reserve.min(u.capacity));
    let (root, default) = (id(ROOT), id(DEFAULT));
    let (handle, default_minor) = (qdisc_handle(), format!("{DEFAULT:x}"));
    let mut c = Vec::new();
    if !present {
        let words = ["qdisc", "replace", "dev", dev, "root", "handle", &handle, "htb"];
        c.push(args(&words).into_iter().chain(args(&["default", &default_minor])).collect());
    }
    let mut class = |parent: &str, classid: &str, rate: &str, ceil: &str| {
        let words = ["class", "replace", "dev", dev, "parent", parent, "classid", classid, "htb"];
        let mut a = args(&words);
        a.extend(args(&["rate", rate, "ceil", ceil, "quantum", QUANTUM]));
        c.push(a);
    };
    class(&handle, &root, &cap, &cap);
    class(&root, &default, &reserve, &cap);
    c.push(args(&["qdisc", "replace", "dev", dev, "parent", &default, "fq_codel"]));
    c
}

/// The commands giving a workload class `minor` on `dev`, its rate `reserved` and its ceiling
/// `ceil`.
pub(crate) fn class(dev: &str, minor: u32, (reserved, ceil): (u64, u64)) -> Vec<Vec<String>> {
    let (classid, root) = (id(minor), id(ROOT));
    let mut a = args(&["class", "replace", "dev", dev, "parent", &root, "classid", &classid]);
    let (rate, ceil) = (rate(reserved), rate(ceil.max(reserved)));
    a.extend(args(&["htb", "rate", &rate, "ceil", &ceil, "quantum", QUANTUM]));
    vec![a, args(&["qdisc", "replace", "dev", dev, "parent", &classid, "fq_codel"])]
}

/// The commands taking class `minor` off `dev`, each of which may find nothing to remove.
pub(crate) fn unclass(dev: &str, minor: u32) -> Vec<Vec<String>> {
    let classid = id(minor);
    vec![
        args(&["qdisc", "del", "dev", dev, "parent", &classid]),
        args(&["class", "del", "dev", dev, "classid", &classid]),
    ]
}

/// The commands holding everything `dev` sends to `limit`, or, for none, removing what held it;
/// `present` when its root qdisc is Korpis's already, which HTB will not change in place.
pub(crate) fn link(dev: &str, limit: u64, present: bool) -> Vec<Vec<String>> {
    if limit == 0 {
        return if present { vec![args(&["qdisc", "del", "dev", dev, "root"])] } else { vec![] };
    }
    let (handle, class) = (qdisc_handle(), id(ROOT));
    let one = format!("{ROOT:x}");
    let r = rate(limit);
    let mut c = Vec::new();
    if !present {
        let words = ["qdisc", "replace", "dev", dev, "root", "handle", &handle, "htb", "default"];
        c.push(args(&words).into_iter().chain([one]).collect());
    }
    c.push(
        args(&["class", "replace", "dev", dev, "parent", &handle, "classid", &class, "htb"])
            .into_iter()
            .chain(args(&["rate", &r, "ceil", &r, "quantum", QUANTUM]))
            .collect(),
    );
    c.push(args(&["qdisc", "replace", "dev", dev, "parent", &class, "fq_codel"]));
    c
}

/// Whether `dev`'s root qdisc is Korpis's, in the calling thread's network namespace.
pub(crate) fn present(dev: &str) -> io::Result<bool> {
    let out = Command::new("tc").args(["-j", "qdisc", "show", "dev", dev]).output()?;
    let listed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let ours = qdisc_handle();
    Ok(listed.as_array().into_iter().flatten().any(|q| {
        q.get("root").and_then(serde_json::Value::as_bool) == Some(true)
            && q.get("handle").and_then(serde_json::Value::as_str) == Some(&ours)
    }))
}

/// Runs each of `commands` with `tc`, in the calling thread's network namespace; with `strict`
/// the first that fails is the error, and without it a failure is a thing already absent.
pub(crate) fn run(commands: &[Vec<String>], strict: bool) -> io::Result<()> {
    for a in commands {
        let out = Command::new("tc").args(a).output()?;
        if strict && !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            return Err(io::Error::other(format!("tc {} refused: {}", a.join(" "), why.trim())));
        }
    }
    Ok(())
}

/// The interfaces whose root qdisc is Korpis's.
pub(crate) fn trees() -> io::Result<Vec<String>> {
    let out = Command::new("tc").args(["-j", "qdisc", "show"]).output()?;
    if !out.status.success() {
        return Err(io::Error::other("tc could not list the qdiscs"));
    }
    let listed: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(io::Error::other)?;
    let ours = qdisc_handle();
    Ok(listed
        .as_array()
        .into_iter()
        .flatten()
        .filter(|q| q.get("root").and_then(serde_json::Value::as_bool) == Some(true))
        .filter(|q| q.get("handle").and_then(serde_json::Value::as_str) == Some(&ours))
        .filter_map(|q| q.get("dev").and_then(serde_json::Value::as_str).map(str::to_owned))
        .collect())
}

/// Whether `tc` answers and the kernel makes an HTB qdisc; why not, when it cannot.
pub(crate) fn offered() -> Result<(), String> {
    match Command::new("tc").arg("-V").output() {
        Ok(out) if out.status.success() => {}
        Ok(_) | Err(_) => return Err("`tc` from iproute2 is not installed".into()),
    }
    let loaded = std::path::Path::new("/sys/module/sch_htb").exists();
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let modules =
        std::path::Path::new("/lib/modules").join(release.trim()).join("kernel/net/sched");
    let available = std::fs::read_dir(modules)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("sch_htb."));
    if loaded || available {
        Ok(())
    } else {
        Err("the kernel has no HTB qdisc (CONFIG_NET_SCH_HTB)".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_uplink_is_a_tree_of_its_capacity_whose_default_keeps_the_node_its_share() {
        let u = Uplink { interface: "eth0".into(), capacity: 1_000_000_000, reserve: 50_000_000 };
        let made = uplink(&u, false);
        assert_eq!(made[0].join(" "), "qdisc replace dev eth0 root handle 4b50: htb default 2");
        assert!(
            made[1].join(" ").contains("classid 4b50:1 htb rate 1000000000bit ceil 1000000000bit")
        );
        assert!(
            made[2].join(" ").contains("classid 4b50:2 htb rate 50000000bit ceil 1000000000bit")
        );
        assert!(!uplink(&u, true).iter().any(|a| a.contains(&"root".to_owned())), "kept");
        let c = class("eth0", 0x10, (100_000_000, 0)).remove(0).join(" ");
        assert!(
            c.ends_with("4b50:10 htb rate 100000000bit ceil 100000000bit quantum 60000"),
            "{c}"
        );
        assert_eq!(link("kwabc", 0, true)[0].join(" "), "qdisc del dev kwabc root");
        assert!(link("kwabc", 0, false).is_empty(), "nothing of Korpis's to remove");
        assert_eq!(link("eth0", 20_000_000, true).len(), 2, "its qdisc kept, its class changed");
        let held = link("eth0", 20_000_000, false).remove(1).join(" ");
        assert!(held.ends_with("rate 20000000bit ceil 20000000bit quantum 60000"), "{held}");
    }
}
