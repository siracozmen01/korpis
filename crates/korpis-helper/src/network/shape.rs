// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bandwidth, shaped (§7 of docs/07-networking.md), written with iproute2's `tc` as the rules are
//! written with `nft`. Every qdisc Korpis makes on an uplink has the handle `4b50:`, and every one
//! on a workload's link `4b51:`, so a tree of its own is told apart from another program's, and an
//! uplink's from a link's, and found again without a ledger. An uplink's tree is a root
//! class of the uplink's capacity, a default class whose floor is what the node keeps for itself,
//! and a class for each workload that reserves, its rate the reservation and its ceiling the
//! limit. A workload's link holds its limit on everything it sends, on `eth0` inside its
//! namespace, and its ingress limit on the host's side, with a class beneath each for an endpoint
//! with a limit of its own. Each leaf queues by
//! `fq_codel`, so one flow's backlog does not delay the others behind it.

use korpis_core::workload::network::HOST_LINK_PREFIX;
use std::io;
use std::process::Command;

/// The major number of every qdisc Korpis makes on an uplink, in hexadecimal as `tc` writes
/// handles.
pub(crate) const MAJOR: u32 = 0x4b50;
/// The major number of every qdisc Korpis makes on a workload's link: another than an uplink's,
/// so neither is ever taken for the other, and a priority set for an uplink's class names no
/// class of a link.
pub(crate) const LINK: u32 = 0x4b51;
/// The class of an uplink's whole capacity.
const ROOT: u32 = 1;
/// The class of the node's own traffic and of whatever reserves nothing.
const DEFAULT: u32 = 2;
/// The first class a workload is given on an uplink, and an endpoint on its workload's link.
pub(crate) const FIRST: u32 = 0x10;
/// A ceiling for a class that has none of its own: above any link's capacity, so its parent's,
/// or the link's, is what holds it.
pub(crate) const NO_CEILING: u64 = 10_000_000_000_000;
/// The bytes each class may send in its turn: some forty full frames, where HTB's own, its rate
/// over ten, is megabytes at gigabit rates and lets one class send a burst that long.
const QUANTUM: &str = "60000";

/// The handle of an uplink's class `minor`, as `tc` and nftables write it: `4b50:10`.
pub(crate) fn id(minor: u32) -> String {
    format!("{MAJOR:x}:{minor:x}")
}

/// The handle of a workload link's class `minor`: `4b51:10`.
pub(crate) fn link_id(minor: u32) -> String {
    format!("{LINK:x}:{minor:x}")
}

fn qdisc_handle() -> String {
    format!("{MAJOR:x}:")
}

fn link_handle() -> String {
    format!("{LINK:x}:")
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

/// The commands holding everything `dev` sends to `limit`, and what each endpoint of
/// `endpoints` sends to its own ceiling in a class of its own, its minor and ceiling; or, for
/// neither, removing what held them. `present` names the classes beneath Korpis's root qdisc on
/// `dev` when there is one, which HTB will not change in place: the root and the default are
/// changed, and an endpoint's class no longer wanted is removed.
pub(crate) fn link(
    dev: &str,
    limit: u64,
    endpoints: &[(u32, u64)],
    present: Option<&[u32]>,
) -> Vec<Vec<String>> {
    if limit == 0 && endpoints.is_empty() {
        return present
            .map_or_else(Vec::new, |_| vec![args(&["qdisc", "del", "dev", dev, "root"])]);
    }
    let handle = link_handle();
    let top = if limit == 0 { NO_CEILING } else { limit };
    let share = top / u64::try_from(endpoints.len() + 1).unwrap_or(u64::MAX);
    let mut c = Vec::new();
    if present.is_none() {
        let words = ["qdisc", "replace", "dev", dev, "root", "handle", &handle, "htb", "default"];
        c.push(args(&words).into_iter().chain([format!("{DEFAULT:x}")]).collect());
    }
    let mut class = |parent: &str, minor: u32, (rate_bits, ceil_bits): (u64, u64)| {
        let classid = link_id(minor);
        let mut a = args(&["class", "replace", "dev", dev, "parent", parent, "classid", &classid]);
        let (r, ceil) = (rate(rate_bits.min(ceil_bits)), rate(ceil_bits));
        a.extend(args(&["htb", "rate", &r, "ceil", &ceil, "quantum", QUANTUM]));
        c.push(a);
        if minor != ROOT {
            c.push(args(&["qdisc", "replace", "dev", dev, "parent", &classid, "fq_codel"]));
        }
    };
    class(&handle, ROOT, (top, top));
    let root = link_id(ROOT);
    class(&root, DEFAULT, (share, top));
    for (minor, ceil) in endpoints {
        class(&root, *minor, (share, (*ceil).min(top)));
    }
    let wanted = |m: &u32| *m == ROOT || *m == DEFAULT || endpoints.iter().any(|(e, _)| e == m);
    for stale in present.unwrap_or_default().iter().filter(|m| !wanted(m)) {
        c.push(args(&["class", "del", "dev", dev, "classid", &link_id(*stale)]));
    }
    c
}

/// The classes beneath Korpis's root qdisc on the workload's link `dev`, in the calling thread's
/// network namespace, by minor; none when its root qdisc is not Korpis's.
pub(crate) fn held(dev: &str) -> io::Result<Option<Vec<u32>>> {
    if !present(dev)? {
        return Ok(None);
    }
    let out = Command::new("tc").args(["class", "show", "dev", dev]).output()?;
    let prefix = link_handle();
    let text = String::from_utf8_lossy(&out.stdout);
    let minors = text
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2)?.strip_prefix(&prefix))
        .filter_map(|m| u32::from_str_radix(m, 16).ok())
        .collect();
    Ok(Some(minors))
}

/// Whether the root qdisc of the workload's link `dev` is Korpis's, in the calling thread's
/// network namespace.
pub(crate) fn present(dev: &str) -> io::Result<bool> {
    let out = Command::new("tc").args(["-j", "qdisc", "show", "dev", dev]).output()?;
    let listed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let ours = link_handle();
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

/// The uplinks whose root qdisc is Korpis's: never a workload's link, whose qdisc has a major of
/// its own and whose name is one no uplink is given.
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
        .filter(|dev| !dev.starts_with(HOST_LINK_PREFIX))
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
        assert_eq!(link("kwabc", 0, &[], Some(&[1, 2]))[0].join(" "), "qdisc del dev kwabc root");
        assert!(link("kwabc", 0, &[], None).is_empty(), "nothing of Korpis's to remove");
        let made = link("eth0", 20_000_000, &[], None);
        assert_eq!(made[0].join(" "), "qdisc replace dev eth0 root handle 4b51: htb default 2");
        let root = made[1].join(" ");
        assert!(root.ends_with("4b51:1 htb rate 20000000bit ceil 20000000bit quantum 60000"));
        assert!(made[2].join(" ").contains("4b51:2 htb rate 20000000bit ceil 20000000bit"));
        assert!(
            !link("eth0", 20_000_000, &[], Some(&[1, 2]))
                .iter()
                .any(|a| a[0] == "qdisc" && a[1] == "replace" && a.contains(&"root".to_owned())),
            "its qdisc kept"
        );
    }

    #[test]
    fn an_endpoints_limit_is_a_class_on_its_workloads_link_and_one_no_longer_wanted_goes() {
        let made = link("eth0", 0, &[(0x10, 5_000_000)], Some(&[1, 2, 0x10, 0x11]));
        let words: Vec<String> = made.iter().map(|a| a.join(" ")).collect();
        let ceiling = format!("4b51:1 htb rate {NO_CEILING}bit ceil {NO_CEILING}bit");
        assert!(words.iter().any(|w| w.contains(&ceiling)), "no limit of its own: {words:?}");
        assert!(words.iter().any(|w| w.contains("4b51:10 htb rate 5000000bit ceil 5000000bit")));
        assert!(words.iter().any(|w| w == "class del dev eth0 classid 4b51:11"), "{words:?}");
        assert!(!words.iter().any(|w| w.contains("classid 4b51:10") && w.contains("del")));
    }
}
