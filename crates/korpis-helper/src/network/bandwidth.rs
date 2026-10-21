// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's bandwidth as a request asks for it, checked, and made so on the node's uplinks
//! and its link (§7 of docs/07-networking.md); and the uplinks themselves.

use super::account::{self, Listed};
use super::shape::{self, Uplink};
use super::{INNER, netns};
use crate::error::{HelperError, HelperResult, identifier};
use korpis_proto::korpis::helper::v1 as helper;
use std::path::Path;

/// A ceiling for a class that has none of its own: above any uplink's capacity, so its
/// parent's is what holds it.
const NO_CEILING: u64 = 10_000_000_000_000;

/// What a workload may send and be sent, in bits a second, as the kernel is to hold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Shaping {
    /// Held on the uplinks, in a class of its own; zero for none.
    pub(crate) reserved: u64,
    /// The ceiling on the uplinks and on its link; zero for none.
    pub(crate) limit: u64,
    /// What its link is sent at most; zero for none.
    pub(crate) ingress: u64,
    /// Nothing it sends leaves for beyond the node.
    pub(crate) cut_off: bool,
}

/// What `b` asks for, with a spent budget's floor in place of the limit.
pub(crate) fn asked(b: Option<&helper::Bandwidth>) -> HelperResult<Shaping> {
    let Some(b) = b else { return Ok(Shaping::default()) };
    if b.egress_limit > 0 && b.egress_reservation > b.egress_limit {
        return Err(HelperError::Refused("an egress reservation is above its limit".into()));
    }
    let mut s = Shaping {
        reserved: b.egress_reservation,
        limit: b.egress_limit,
        ingress: b.ingress_limit,
        cut_off: b.exhausted && b.cut_off,
    };
    if b.exhausted && !b.cut_off {
        if b.floor == 0 {
            return Err(HelperError::Refused("a spent budget names a floor or a cut-off".into()));
        }
        s.limit = if s.limit == 0 { b.floor } else { s.limit.min(b.floor) };
        s.reserved = s.reserved.min(b.floor);
    }
    Ok(s)
}

/// The class `link` is to hold on the uplinks, if it reserves.
pub(crate) fn class(link: &str, s: &Shaping, listed: &Listed) -> HelperResult<Option<u32>> {
    if s.reserved == 0 {
        return Ok(None);
    }
    account::class_for(link, listed, shape::FIRST)
        .map(Some)
        .ok_or_else(|| HelperError::Failed("every class of the node's uplinks is held".into()))
}

/// Makes `s` so for the workload whose link is `link` and whose namespaces are in `dir`, its
/// class `class`, after its accounting rows are written: its class on every uplink, the
/// ingress limit on the host's side of its link, and its limit inside.
pub(crate) fn apply(dir: &Path, link: &str, s: &Shaping, class: Option<u32>) -> HelperResult<()> {
    if let Some(minor) = class {
        let ceil = if s.limit == 0 { NO_CEILING } else { s.limit };
        for dev in shape::trees()? {
            shape::run(&shape::class(&dev, minor, (s.reserved, ceil)), true)?;
        }
    }
    shape::run(&shape::link(link, s.ingress, shape::present(link)?), true)?;
    let inside = || shape::run(&shape::link(INNER, s.limit, shape::present(INNER)?), true);
    netns::within(&netns::net(dir), inside)?;
    Ok(())
}

/// Takes `link`'s class, if it held one, off every uplink: on release, and when it stops
/// reserving.
pub(crate) fn unclass(link: &str, listed: &Listed) -> HelperResult<()> {
    let Some((_, minor)) = listed.classes.iter().find(|(l, _)| l == link) else { return Ok(()) };
    for dev in shape::trees()? {
        shape::run(&shape::unclass(&dev, *minor), false)?;
    }
    Ok(())
}

/// Makes the node's uplinks `r` names shaped, and removes the trees of any it does not.
pub(crate) fn uplinks(
    r: &helper::EnsureUplinksRequest,
) -> HelperResult<helper::EnsureUplinksResponse> {
    shape::offered().map_err(HelperError::Failed)?;
    let mut wanted = Vec::new();
    for u in &r.uplinks {
        identifier("uplink", &u.interface)?;
        if u.interface.len() > 15 || u.interface.starts_with("kw") {
            return Err(HelperError::Refused(format!("{} is not an uplink", u.interface)));
        }
        if u.capacity == 0 || u.reserve >= u.capacity {
            return Err(HelperError::Refused(format!(
                "uplink {}: a capacity, and a reserve below it",
                u.interface
            )));
        }
        let (capacity, reserve) = (u.capacity, u.reserve);
        wanted.push(Uplink { interface: u.interface.clone(), capacity, reserve });
    }
    let present = shape::trees()?;
    let mut removed = Vec::new();
    for dev in present.iter().filter(|d| !wanted.iter().any(|u| &u.interface == *d)) {
        shape::run(&shape::link(dev, 0, true), true)?;
        removed.push(dev.clone());
    }
    for u in &wanted {
        shape::run(&shape::uplink(u, present.contains(&u.interface)), true)?;
    }
    Ok(helper::EnsureUplinksResponse {
        shaped: wanted.into_iter().map(|u| u.interface).collect(),
        removed,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spent_budget_holds_its_workload_to_its_floor_or_cuts_it_off() {
        let b = |exhausted, floor, cut_off| helper::Bandwidth {
            egress_reservation: 100,
            egress_limit: 1000,
            exhausted,
            floor,
            cut_off,
            ..Default::default()
        };
        assert_eq!(asked(Some(&b(false, 0, false))).unwrap().limit, 1000);
        let floored = asked(Some(&b(true, 50, false))).unwrap();
        assert_eq!((floored.reserved, floored.limit, floored.cut_off), (50, 50, false));
        assert!(asked(Some(&b(true, 0, true))).unwrap().cut_off);
        assert!(asked(Some(&b(true, 0, false))).is_err(), "spent, and nothing said what then");
        let above =
            helper::Bandwidth { egress_reservation: 9, egress_limit: 5, ..Default::default() };
        assert!(asked(Some(&above)).is_err());
        assert_eq!(asked(None).unwrap(), Shaping::default());
    }
}
